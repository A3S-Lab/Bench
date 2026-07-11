"""A3S Bench protected text-adventure Judge server.

Adapted from ByteDance Seed EdgeBench at the pinned provenance revision.
Licensed under Apache-2.0; see builtin/licenses/Apache-2.0.txt.
"""

import argparse
import threading

import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel
import jericho


class NewGameRequest(BaseModel):
    pass


class StepRequest(BaseModel):
    action: str


class State:
    def __init__(self, rom):
        self.rom = rom
        self.env = None
        self.moves = 0
        self.score = 0
        self.peak = 0
        self.maximum = 0
        self.done = False
        self.lock = threading.Lock()


def response(state, observation=""):
    return {
        "observation": observation,
        "score": state.score,
        "peak_score": state.peak,
        "max_score": state.maximum,
        "done": state.done,
        "moves": state.moves,
    }


def create_app(rom):
    app = FastAPI(title="A3S Bench protected game Judge")
    state = State(rom)

    @app.get("/health")
    def health():
        return {"ok": True}

    @app.post("/new")
    def new_game(_request: NewGameRequest):
        with state.lock:
            if state.env is not None:
                state.env.close()
            state.env = jericho.FrotzEnv(state.rom)
            observation, _ = state.env.reset()
            state.moves = 0
            state.score = 0
            state.peak = 0
            state.maximum = int(state.env.get_max_score() or 0)
            state.done = False
            return response(state, observation)

    @app.post("/step")
    def step(request: StepRequest):
        with state.lock:
            if state.env is None:
                raise HTTPException(400, "call /new first")
            if state.done:
                raise HTTPException(400, "game is already over")
            observation, _, state.done, _ = state.env.step(request.action)
            state.moves += 1
            state.score = int(state.env.get_score() or 0)
            state.peak = max(state.peak, state.score)
            return response(state, observation)

    @app.get("/status")
    def status():
        with state.lock:
            return response(state)

    @app.post("/close")
    def close():
        with state.lock:
            result = {
                "final_score": state.score,
                "peak_score": state.peak,
                "max_score": state.maximum,
                "moves": state.moves,
            }
            if state.env is not None:
                state.env.close()
                state.env = None
            state.done = True
            return result

    return app


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--rom", required=True)
    parser.add_argument("--port", type=int, default=8000)
    args = parser.parse_args()
    uvicorn.run(create_app(args.rom), host="0.0.0.0", port=args.port, log_level="warning")


if __name__ == "__main__":
    main()
