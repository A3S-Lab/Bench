use anyhow::Result;
use serde::Serialize;

pub fn print_success(command: &str, data: impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(&success(command, data))?);
    Ok(())
}

pub fn print_error(command: &str, message: &str) {
    println!("{}", error(command, message));
}

fn success(command: &str, data: impl Serialize) -> serde_json::Value {
    serde_json::json!({
        "schema": "a3s.bench.output.v1",
        "command": command,
        "ok": true,
        "data": data,
    })
}

fn error(command: &str, message: &str) -> serde_json::Value {
    serde_json::json!({
        "schema": "a3s.bench.output.v1",
        "command": command,
        "ok": false,
        "error": {
            "code": "command_failed",
            "message": message,
        },
    })
}

pub fn command_name(args: &[String]) -> String {
    match args {
        [advanced, group, action, ..] if advanced == "advanced" => {
            format!("advanced {group} {action}")
        }
        [advanced, action, ..] if advanced == "advanced" => format!("advanced {action}"),
        [command, ..] => command.clone(),
        [] => "help".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_stable_command_names() {
        assert_eq!(command_name(&["list".into(), "--json".into()]), "list");
        assert_eq!(
            command_name(&["advanced".into(), "candidate".into(), "lock".into()]),
            "advanced candidate lock"
        );
    }

    #[test]
    fn envelopes_have_closed_top_level_shapes() {
        let success = success("list", serde_json::json!({"tasks":[]}));
        let error = error("run", "invalid input");
        assert_eq!(
            success
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["command", "data", "ok", "schema"]
        );
        assert_eq!(
            error
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["command", "error", "ok", "schema"]
        );
    }
}
