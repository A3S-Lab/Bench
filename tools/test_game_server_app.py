import sys
import threading
import types
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

sys.modules.setdefault("jericho", types.SimpleNamespace(FrotzEnv=object))

from fastapi import HTTPException

from starlette.requests import Request
from runtime_assets import game_server_app as server


class FakeEnv:
    instances = []

    def __init__(self, _rom):
        self.score = 0
        self.closed = 0
        FakeEnv.instances.append(self)

    def reset(self):
        return "start", {}

    def get_max_score(self):
        return 100

    def step(self, action):
        done = False
        if action == "gain":
            self.score = 20
        elif action == "finish-loss":
            self.score = 5
            done = True
        elif action == "negative":
            self.score = -3
        return action, 0, done, {}

    def get_score(self):
        return self.score

    def close(self):
        self.closed += 1


class FailingResetEnv(FakeEnv):
    def reset(self):
        raise RuntimeError("reset failed")

class FailingMaxScoreEnv(FakeEnv):
    def get_max_score(self):
        raise RuntimeError("max score failed")


class ParallelEnv(FakeEnv):
    barrier = None
    counter_lock = threading.Lock()
    active = 0
    max_active = 0

    def step(self, action):
        with type(self).counter_lock:
            type(self).active += 1
            type(self).max_active = max(type(self).max_active, type(self).active)
        try:
            type(self).barrier.wait(timeout=2)
            return super().step(action)
        finally:
            with type(self).counter_lock:
                type(self).active -= 1


def endpoint(app, path, method):
    for route in app.routes:
        if getattr(route, "path", None) == path and method in getattr(route, "methods", set()):
            return route.endpoint
    raise AssertionError(f"missing {method} {path}")

def http_request(client_host="127.0.0.1"):
    return Request(
        {
            "type": "http",
            "method": "GET",
            "path": "/",
            "headers": [],
            "client": (client_host, 12345),
        }
    )


class GameServerTests(unittest.TestCase):
    def setUp(self):
        FakeEnv.instances = []

    def operations(self, app):
        close_all = endpoint(app, "/close-all", "POST")
        history = endpoint(app, "/history", "GET")
        return {
            "new": endpoint(app, "/new", "POST"),
            "step": endpoint(app, "/{session_id}/step", "POST"),
            "status": endpoint(app, "/{session_id}/status", "GET"),
            "close": endpoint(app, "/{session_id}/close", "POST"),
            "close_all": lambda: close_all(http_request()),
            "history": lambda: history(http_request()),
        }

    def test_sessions_are_isolated_and_fourth_new_archives_oldest(self):
        app = server.create_app("game.z5", env_factory=FakeEnv)
        op = self.operations(app)

        raw_history = endpoint(app, "/history", "GET")
        with self.assertRaises(HTTPException) as error:
            raw_history(http_request("172.18.0.2"))
        self.assertEqual(error.exception.status_code, 404)
        self.assertFalse(
            next(route for route in app.routes if getattr(route, "path", None) == "/history").include_in_schema
        )

        first = op["new"](server.NewGameRequest())
        op["step"](first["session_id"], server.StepRequest(action="gain"))
        second = op["new"](server.NewGameRequest())
        third = op["new"](server.NewGameRequest())
        fourth = op["new"](server.NewGameRequest())

        history = op["history"]()
        self.assertEqual([entry["session_id"] for entry in history["entries"]], [first["session_id"]])
        self.assertEqual(history["best_score"], 20)
        self.assertEqual(op["status"](second["session_id"])["score"], 0)
        self.assertEqual(op["status"](third["session_id"])["score"], 0)
        self.assertEqual(op["status"](fourth["session_id"])["score"], 0)
        with self.assertRaises(HTTPException) as error:
            op["status"](first["session_id"])
        self.assertEqual(error.exception.status_code, 404)

        op["close"](second["session_id"])
        self.assertEqual(op["close_all"](), {"closed": 2})
        self.assertEqual(op["close_all"](), {"closed": 0})
        self.assertEqual(len(op["history"]()["entries"]), 4)
        self.assertTrue(all(env.closed == 1 for env in FakeEnv.instances))

    def test_single_session_mode_archives_previous_game(self):
        app = server.create_app(
            "game.z5",
            env_factory=FakeEnv,
            max_active_sessions=1,
        )
        op = self.operations(app)
        first = op["new"](server.NewGameRequest())
        op["step"](first["session_id"], server.StepRequest(action="gain"))
        second = op["new"](server.NewGameRequest())

        history = op["history"]()
        self.assertEqual(
            [entry["session_id"] for entry in history["entries"]],
            [first["session_id"]],
        )
        self.assertEqual(history["best_score"], 20)
        with self.assertRaises(HTTPException):
            op["status"](first["session_id"])
        self.assertEqual(op["status"](second["session_id"])["score"], 0)
        self.assertEqual(op["close_all"](), {"closed": 1})

    def test_concurrent_new_requests_honor_single_session_limit(self):
        app = server.create_app(
            "game.z5",
            env_factory=FakeEnv,
            max_active_sessions=1,
        )
        op = self.operations(app)
        with ThreadPoolExecutor(max_workers=4) as pool:
            games = [
                future.result(timeout=3)
                for future in [
                    pool.submit(op["new"], server.NewGameRequest())
                    for _ in range(4)
                ]
            ]

        active = 0
        for game in games:
            try:
                op["status"](game["session_id"])
                active += 1
            except HTTPException as error:
                self.assertEqual(error.status_code, 404)
        self.assertEqual(active, 1)
        self.assertEqual(len(op["history"]()["entries"]), 3)
        self.assertEqual(op["close_all"](), {"closed": 1})

    def test_steps_for_independent_sessions_can_run_in_parallel(self):
        ParallelEnv.barrier = threading.Barrier(2)
        ParallelEnv.active = 0
        ParallelEnv.max_active = 0
        app = server.create_app("game.z5", env_factory=ParallelEnv)
        op = self.operations(app)
        first = op["new"](server.NewGameRequest())
        second = op["new"](server.NewGameRequest())

        with ThreadPoolExecutor(max_workers=2) as pool:
            futures = [
                pool.submit(op["step"], first["session_id"], server.StepRequest(action="gain")),
                pool.submit(op["step"], second["session_id"], server.StepRequest(action="gain")),
            ]
            results = [future.result(timeout=3) for future in futures]

        self.assertEqual([result["score"] for result in results], [20, 20])
        self.assertEqual(ParallelEnv.max_active, 2)

    def test_done_session_archives_final_score_not_peak(self):
        app = server.create_app("game.z5", env_factory=FakeEnv)
        op = self.operations(app)
        game = op["new"](server.NewGameRequest())
        op["step"](game["session_id"], server.StepRequest(action="gain"))
        result = op["step"](
            game["session_id"],
            server.StepRequest(action="finish-loss"),
        )

        self.assertTrue(result["done"])
        self.assertEqual(result["score"], 5)
        self.assertEqual(result["peak_score"], 20)
        history = op["history"]()
        self.assertEqual(history["best_score"], 5)
        self.assertEqual(history["entries"][0]["final_score"], 5)
        self.assertEqual(history["entries"][0]["peak_score"], 20)
        with self.assertRaises(HTTPException):
            op["status"](game["session_id"])

    def test_failed_reset_does_not_discard_active_session(self):
        factories = iter([FakeEnv("game.z5"), FailingResetEnv("game.z5")])
        app = server.create_app("game.z5", env_factory=lambda _rom: next(factories))
        op = self.operations(app)
        game = op["new"](server.NewGameRequest())

        with self.assertRaises(RuntimeError):
            op["new"](server.NewGameRequest())
        self.assertEqual(op["status"](game["session_id"])["score"], 0)
        self.assertEqual(op["history"]()["entries"], [])
        self.assertEqual(op["history"]()["best_round"], "")

    def test_failed_max_score_lookup_keeps_official_eviction_order(self):
        factories = iter(
            [
                FakeEnv("game.z5"),
                FakeEnv("game.z5"),
                FakeEnv("game.z5"),
                FailingMaxScoreEnv("game.z5"),
            ]
        )
        app = server.create_app("game.z5", env_factory=lambda _rom: next(factories))
        op = self.operations(app)
        active = [op["new"](server.NewGameRequest()) for _ in range(3)]

        with self.assertRaises(RuntimeError):
            op["new"](server.NewGameRequest())
        with self.assertRaises(HTTPException) as error:
            op["status"](active[0]["session_id"])
        self.assertEqual(error.exception.status_code, 404)
        self.assertTrue(all(op["status"](game["session_id"]) for game in active[1:]))
        history = op["history"]()
        self.assertEqual([entry["session_id"] for entry in history["entries"]], [active[0]["session_id"]])
        self.assertEqual(history["entries"][0]["round"], "game-1")
        self.assertEqual(FakeEnv.instances[-1].closed, 1)


if __name__ == "__main__":
    unittest.main()
