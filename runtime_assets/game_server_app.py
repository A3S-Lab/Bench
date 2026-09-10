"""A3S Bench protected text-adventure Judge server.

Adapted from ByteDance Seed EdgeBench at the pinned provenance revision.
Licensed under Apache-2.0; see builtin/licenses/Apache-2.0.txt.
"""

import argparse
import threading
import uuid

import jericho
import uvicorn
from fastapi import FastAPI, HTTPException, Request
from pydantic import BaseModel


MAX_ACTIVE_SESSIONS = 3


class NewGameRequest(BaseModel):
    pass


class StepRequest(BaseModel):
    action: str


class GameSessionState:
    def __init__(self, session_id, game_number, env, observation, maximum):
        self.session_id = session_id
        self.game_number = game_number
        self.env = env
        self.observation = observation
        self.moves = 0
        self.score = 0
        self.peak = 0
        self.maximum = maximum
        self.done = False
        self.steps = []
        # EdgeBench serializes operations inside each one-session container.
        self.lock = threading.Lock()


class State:
    def __init__(self, rom, env_factory, max_active_sessions):
        self.rom = rom
        self.env_factory = env_factory
        self.max_active_sessions = max_active_sessions
        self.sessions = {}
        self.history = []
        self.next_game_number = 1
        # Serialize session creation so concurrent /new requests cannot exceed
        # the configured active-session capacity while an environment resets.
        self.new_lock = threading.Lock()
        # Mirrors the host Judge's lock around its session registry.
        self.lock = threading.Lock()


def session_response(session, observation=None):
    return {
        "session_id": session.session_id,
        "observation": session.observation if observation is None else observation,
        "score": session.score,
        "peak_score": session.peak,
        "max_score": session.maximum,
        "done": session.done,
        "moves": session.moves,
    }


def status_response(session):
    return {
        "session_id": session.session_id,
        "score": session.score,
        "peak_score": session.peak,
        "max_score": session.maximum,
        "done": session.done,
        "moves": session.moves,
    }


def close_response(session):
    return {
        "session_id": session.session_id,
        "final_score": session.score,
        "peak_score": session.peak,
        "max_score": session.maximum,
        "moves": session.moves,
    }


def archive_session(state, session):
    with session.lock:
        final_score = session.score
        pass_rate = final_score / session.maximum if session.maximum > 0 else 0.0
        entry = {
            "type": "game",
            "round": f"game-{session.game_number}",
            "session_id": session.session_id,
            "score": final_score,
            "final_score": final_score,
            "peak_score": session.peak,
            "max_score": session.maximum,
            "moves": session.moves,
            "steps": list(session.steps),
            "pass_rate": pass_rate,
        }
        if session.env is not None:
            try:
                session.env.close()
            except Exception:
                pass
            session.env = None
    with state.lock:
        state.history.append(entry)
    return entry


def history_response(state):
    if not state.history:
        return {
            "best_score": 0,
            "best_pass_rate": 0.0,
            "best_round": "",
            "entries": [],
        }
    best = max(state.history, key=lambda entry: entry["score"])
    return {
        "best_score": best["score"],
        "best_pass_rate": best["pass_rate"],
        "best_round": best["round"],
        "entries": list(state.history),
    }


def require_internal_request(request):
    client_host = request.client.host if request.client is not None else None
    if client_host not in {"127.0.0.1", "::1"}:
        raise HTTPException(404, "not found")


def create_app(
    rom,
    env_factory=jericho.FrotzEnv,
    max_active_sessions=MAX_ACTIVE_SESSIONS,
):
    if max_active_sessions < 1:
        raise ValueError("max_active_sessions must be at least 1")
    app = FastAPI(title="A3S Bench protected game Judge")
    state = State(rom, env_factory, max_active_sessions)

    def active_session(session_id):
        session = state.sessions.get(session_id)
        if session is None:
            raise HTTPException(404, "unknown or archived session")
        return session

    @app.get("/health")
    def health():
        return {"ok": True}

    @app.post("/new")
    def new_game(_request: NewGameRequest):
        with state.new_lock:
            # Apply the capacity decision before the potentially slow environment
            # startup, matching the host Judge's eviction-before-creation ordering.
            with state.lock:
                session_id = uuid.uuid4().hex[:12]
                while session_id in state.sessions:
                    session_id = uuid.uuid4().hex[:12]
                oldest = None
                if len(state.sessions) >= state.max_active_sessions:
                    oldest_id = next(iter(state.sessions))
                    oldest = state.sessions.pop(oldest_id)
            if oldest is not None:
                archive_session(state, oldest)

            env = state.env_factory(state.rom)
            try:
                observation, _ = env.reset()
                maximum = int(env.get_max_score() or 0)
            except Exception:
                try:
                    env.close()
                except Exception:
                    pass
                raise
            with state.lock:
                game_number = state.next_game_number
                state.next_game_number += 1
                session = GameSessionState(
                    session_id,
                    game_number,
                    env,
                    observation,
                    maximum,
                )
                state.sessions[session_id] = session
            return session_response(session)

    @app.post("/{session_id}/step")
    def step(session_id: str, request: StepRequest):
        with state.lock:
            session = active_session(session_id)
        with session.lock:
            if session.done or session.env is None:
                raise HTTPException(400, "game is already over")
            observation, _, session.done, _ = session.env.step(request.action)
            session.observation = observation
            session.moves += 1
            session.score = int(session.env.get_score() or 0)
            session.peak = max(session.peak, session.score)
            result = session_response(session, observation)
            session.steps.append(
                {
                    "move": session.moves,
                    "action": request.action,
                    "observation": observation,
                    "score": session.score,
                    "peak_score": session.peak,
                    "max_score": session.maximum,
                    "done": session.done,
                }
            )
            done = session.done
        if done:
            with state.lock:
                if state.sessions.get(session_id) is session:
                    state.sessions.pop(session_id)
                    archive = True
                else:
                    archive = False
            if archive:
                archive_session(state, session)
        return result

    @app.get("/{session_id}/status")
    def status(session_id: str):
        with state.lock:
            session = active_session(session_id)
        with session.lock:
            return status_response(session)

    @app.post("/{session_id}/close")
    def close(session_id: str):
        with state.lock:
            session = active_session(session_id)
            state.sessions.pop(session_id)
        archive_session(state, session)
        return close_response(session)

    @app.post("/close-all", include_in_schema=False)
    def close_all(request: Request):
        require_internal_request(request)
        with state.lock:
            sessions = list(state.sessions.values())
            state.sessions.clear()
        for session in sessions:
            archive_session(state, session)
        return {"closed": len(sessions)}

    @app.get("/history", include_in_schema=False)
    def history(request: Request):
        require_internal_request(request)
        with state.lock:
            return history_response(state)

    return app


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--rom", required=True)
    parser.add_argument("--port", type=int, default=8000)
    parser.add_argument(
        "--max-active-sessions",
        type=int,
        default=MAX_ACTIVE_SESSIONS,
    )
    args = parser.parse_args()
    if args.max_active_sessions < 1:
        parser.error("--max-active-sessions must be at least 1")
    uvicorn.run(
        create_app(args.rom, max_active_sessions=args.max_active_sessions),
        host="0.0.0.0",
        port=args.port,
        log_level="warning",
    )


if __name__ == "__main__":
    main()
