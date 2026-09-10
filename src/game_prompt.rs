pub fn candidate_prompt(
    instructions: &str,
    task_prompt: &str,
    internet: bool,
    parallel_game_sessions: bool,
) -> String {
    let mut prompt = String::with_capacity(instructions.len() + task_prompt.len() + 2_400);
    prompt.push_str(instructions);
    prompt.push_str("\n\n");
    prompt.push_str(
        "## Game Mode\n\n\
You are playing an **interactive fiction game** via an HTTP API. Send commands to the game server and maximize your score.\n\n\
---\n\n",
    );
    if !internet {
        prompt.push_str(
            "## Network Environment\n\n\
**This environment has NO internet access.** Only the game server (\x60GAME_SERVER_URL\x60) and the AI API are reachable; everything else is iptables-blocked. **Do not** attempt to search the web, fetch walkthroughs, or download external resources — those calls will time out and waste your turns. Solve the game through gameplay.\n\n\
---\n\n",
        );
    }
    prompt.push_str(
        "## Game Server HTTP API\n\n\
The server URL is available in the \x60GAME_SERVER_URL\x60 environment variable.\n\n\
### Start a new game\n\
\x60\x60\x60\n\
POST {GAME_SERVER_URL}/new\n\
Body: {}\n\
Response: {\\\"session_id\\\": \\\"abc123\\\", \\\"observation\\\": \\\"...\\\", \\\"score\\\": 0, \\\"peak_score\\\": 0, \\\"max_score\\\": 350, \\\"done\\\": false, \\\"moves\\\": 0}\n\
\x60\x60\x60\n\n\
### Take an action\n\
\x60\x60\x60\n\
POST {GAME_SERVER_URL}/{session_id}/step\n\
Body: {\\\"action\\\": \\\"go north\\\"}\n\
Response: {\\\"session_id\\\": \\\"...\\\", \\\"observation\\\": \\\"...\\\", \\\"score\\\": 5, \\\"peak_score\\\": 5, \\\"max_score\\\": 350, \\\"done\\\": false, \\\"moves\\\": 1}\n\
\x60\x60\x60\n\n\
### Check status\n\
\x60\x60\x60\n\
GET {GAME_SERVER_URL}/{session_id}/status\n\
Response: {\\\"session_id\\\": \\\"...\\\", \\\"score\\\": 5, \\\"peak_score\\\": 5, \\\"max_score\\\": 350, \\\"done\\\": false, \\\"moves\\\": 1}\n\
\x60\x60\x60\n\n\
### Close session\n\
\x60\x60\x60\n\
POST {GAME_SERVER_URL}/{session_id}/close\n\
Response: {\\\"session_id\\\": \\\"...\\\", \\\"final_score\\\": 5, \\\"peak_score\\\": 5, \\\"max_score\\\": 350, \\\"moves\\\": 50}\n\
\x60\x60\x60\n\n\
### Score fields\n\
- \x60score\x60 — the score you currently have in this session.\n\
- \x60peak_score\x60 — the highest score you reached so far in this session (useful in games where score can decrease).\n\
- \x60max_score\x60 — the theoretical maximum score for the game (constant). Your goal is to get \x60score\x60 as close to \x60max_score\x60 as possible.\n\n\
---\n\n\
## Tips\n\n\
- Interactive fiction games accept natural language commands: \\\"go north\\\", \\\"take lamp\\\", \\\"examine door\\\", \\\"open mailbox\\\", \\\"read leaflet\\\", etc.\n\
- \x60look\x60 describes the current room. Moving in a direction that doesn't exist gives a failure message.\n\
- The game has puzzles — read descriptions carefully and experiment.\n\
- You can start multiple game sessions to explore different strategies.\n\
- **Keypress prompts**: Some games pause with prompts like (Press SPACE to Continue) or [press BACKSPACE to return to game]. When you see these in the observation, send a **single space** \\\" \\\" as the next action to dismiss them. These are Z-machine interactive prompts that require a single-character response, not a regular text command.\n\n\
---\n\n\
## Environment\n\n\
- \x60GAME_SERVER_URL\x60 environment variable is pre-set.\n\
- Use \x60curl\x60, \x60urllib.request\x60, or \x60http.client\x60 for HTTP requests.\n\
- Python 3.10 stdlib is available.\n\n\
---\n\n\
## Rules\n\n\
- All game interaction must go through the HTTP API.\n\
- You may start multiple game sessions to explore.\n\
- Maximize your score across all sessions.\n\n\
## Scoring\n\n\
- Your **best score** across all game sessions is your final result, normalized against \x60max_score\x60 (the game's theoretical maximum).\n\
- You don't lose points for failed sessions — experimentation is encouraged.\n\n\
---\n\n",
    );
    if !parallel_game_sessions {
        prompt = prompt
            .replace(
                "- You can start multiple game sessions to explore different strategies.",
                "- Only one game session can be active at a time. Starting a new game archives the current session, so close or finish it before starting another.",
            )
            .replace(
                "- You may start multiple game sessions to explore.",
                "- Keep only one game session active at a time.",
            );
    }
    prompt.push_str(task_prompt);
    prompt.push('\n');
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_prompt_matches_edgebench_game_contract() {
        let prompt = candidate_prompt("candidate instructions", "play carefully", false, true);
        assert!(prompt.starts_with("candidate instructions\n\n## Game Mode"));
        assert!(prompt.contains("## Game Server HTTP API"));
        assert!(prompt.contains("POST {GAME_SERVER_URL}/{session_id}/step"));
        assert!(prompt.contains("You can start multiple game sessions"));
        assert!(prompt.contains("Your **best score** across all game sessions"));
        assert!(prompt.contains("## Network Environment"));
        assert!(!prompt.contains("up to three sessions active"));
        assert!(!prompt.contains("Do not edit workspace files"));
        assert!(prompt.ends_with("play carefully\n"));
    }

    #[test]
    fn game_prompt_omits_offline_note_when_internet_is_available() {
        let prompt = candidate_prompt("instructions", "task", true, true);
        assert!(!prompt.contains("NO internet access"));
    }

    #[test]
    fn serial_game_prompt_forbids_parallel_active_sessions() {
        let prompt = candidate_prompt("instructions", "task", false, false);
        assert!(prompt.contains("Only one game session can be active at a time"));
        assert!(prompt.contains("Keep only one game session active at a time"));
        assert!(!prompt.contains("You can start multiple game sessions"));
        assert!(!prompt.contains("You may start multiple game sessions"));
        assert!(prompt.contains("Your **best score** across all game sessions"));
    }
}
