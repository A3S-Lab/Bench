#!/bin/sh
set -u

if [ "$#" -ne 6 ]; then
    echo "Codex run loop received an invalid argument vector" >&2
    exit 2
fi

codex="$1"
model="$2"
reasoning_effort="$3"
disable_web_search="$4"
preserve_task_proxy="$5"
prompt="$6"
disable_auto_resume=${A3S_CODEX_DISABLE_AUTO_RESUME:-0}
max_resumes=100
min_runtime_for_resume_centiseconds=100
resume_count=0
stop_review_dir=${A3S_CODEX_STOP_REVIEW_DIR:-/tmp/a3s-codex-stop-review}
stop_review_accepted="$stop_review_dir/accepted"

rm -f "$stop_review_dir/convergence-count" "$stop_review_accepted"

monotonic_centiseconds() {
    IFS=' ' read -r uptime_seconds _ </proc/uptime
    whole_seconds=${uptime_seconds%%.*}
    fractional_seconds=${uptime_seconds#*.}
    fractional_seconds=$(printf '%.2s' "${fractional_seconds}00")
    printf '%s\n' "$((whole_seconds * 100 + 1$fractional_seconds - 100))"
}

run_initial() {
    set -- exec -c allow_login_shell=false
    if [ "$preserve_task_proxy" = 1 ]; then
        set -- "$@" \
            -c "shell_environment_policy.set.HTTP_PROXY=\"$HTTP_PROXY\"" \
            -c "shell_environment_policy.set.HTTPS_PROXY=\"$HTTPS_PROXY\"" \
            -c "shell_environment_policy.set.http_proxy=\"$http_proxy\"" \
            -c "shell_environment_policy.set.https_proxy=\"$https_proxy\"" \
            -c "shell_environment_policy.set.NO_PROXY=\"$NO_PROXY\"" \
            -c "shell_environment_policy.set.no_proxy=\"$no_proxy\""
        if [ -n "${PIP_INDEX_URL:-}" ]; then
            set -- "$@" -c "shell_environment_policy.set.PIP_INDEX_URL=\"$PIP_INDEX_URL\""
        fi
        if [ -n "${PIP_TRUSTED_HOST+x}" ]; then
            set -- "$@" -c "shell_environment_policy.set.PIP_TRUSTED_HOST=\"$PIP_TRUSTED_HOST\""
        fi
        if [ -n "${MAVEN_OPTS:-}" ]; then
            set -- "$@" -c "shell_environment_policy.set.MAVEN_OPTS=\"$MAVEN_OPTS\""
        fi
    fi
    if [ "$disable_web_search" = 1 ]; then
        set -- "$@" -c 'web_search="disabled"'
    fi
    set -- "$@" --dangerously-bypass-approvals-and-sandbox --json --skip-git-repo-check -c features.hooks=true
    if [ "$model" = gpt-5.3-codex-spark ]; then
        set -- "$@" --disable image_generation -c model_reasoning_summary=none
    fi
    if [ -n "$model" ]; then
        set -- "$@" --model "$model"
    fi
    if [ -n "$reasoning_effort" ]; then
        set -- "$@" -c "model_reasoning_effort=$reasoning_effort"
    fi
    set -- "$@" -- "$prompt"
    "$codex" "$@"
}

run_resume() {
    set -- exec -c allow_login_shell=false
    if [ "$preserve_task_proxy" = 1 ]; then
        set -- "$@" \
            -c "shell_environment_policy.set.HTTP_PROXY=\"$HTTP_PROXY\"" \
            -c "shell_environment_policy.set.HTTPS_PROXY=\"$HTTPS_PROXY\"" \
            -c "shell_environment_policy.set.http_proxy=\"$http_proxy\"" \
            -c "shell_environment_policy.set.https_proxy=\"$https_proxy\"" \
            -c "shell_environment_policy.set.NO_PROXY=\"$NO_PROXY\"" \
            -c "shell_environment_policy.set.no_proxy=\"$no_proxy\""
        if [ -n "${PIP_INDEX_URL:-}" ]; then
            set -- "$@" -c "shell_environment_policy.set.PIP_INDEX_URL=\"$PIP_INDEX_URL\""
        fi
        if [ -n "${PIP_TRUSTED_HOST+x}" ]; then
            set -- "$@" -c "shell_environment_policy.set.PIP_TRUSTED_HOST=\"$PIP_TRUSTED_HOST\""
        fi
        if [ -n "${MAVEN_OPTS:-}" ]; then
            set -- "$@" -c "shell_environment_policy.set.MAVEN_OPTS=\"$MAVEN_OPTS\""
        fi
    fi
    if [ "$disable_web_search" = 1 ]; then
        set -- "$@" -c 'web_search="disabled"'
    fi
    set -- "$@" resume --last --dangerously-bypass-approvals-and-sandbox --json --skip-git-repo-check -c features.hooks=true
    if [ "$model" = gpt-5.3-codex-spark ]; then
        set -- "$@" --disable image_generation -c model_reasoning_summary=none
    fi
    if [ -n "$model" ]; then
        set -- "$@" --model "$model"
    fi
    if [ -n "$reasoning_effort" ]; then
        set -- "$@" -c "model_reasoning_effort=$reasoning_effort"
    fi
    set -- "$@" 'Continue working.'
    "$codex" "$@"
}

started=$(monotonic_centiseconds)
run_initial
status=$?
elapsed=$(($(monotonic_centiseconds) - started))

while [ ! -f "$stop_review_accepted" ] && [ "$disable_auto_resume" != 1 ] && [ "$elapsed" -ge "$min_runtime_for_resume_centiseconds" ] && [ "$resume_count" -lt "$max_resumes" ]; do
    resume_count=$((resume_count + 1))
    printf 'Resuming Codex (attempt %d/%d)\n' "$resume_count" "$max_resumes" >&2
    started=$(monotonic_centiseconds)
    run_resume
    status=$?
    elapsed=$(($(monotonic_centiseconds) - started))
done

if [ "$disable_auto_resume" = 1 ]; then
    printf 'Codex auto-resume disabled by configuration\n' >&2
elif [ -f "$stop_review_accepted" ]; then
    printf 'Codex stop reviewer accepted repeated convergence claims\n' >&2
elif [ "$elapsed" -lt "$min_runtime_for_resume_centiseconds" ]; then
    printf 'Codex exited after %dms; not resuming a likely systematic failure\n' "$((elapsed * 10))" >&2
elif [ "$resume_count" -ge "$max_resumes" ]; then
    printf 'Codex reached the maximum of %d resume attempts\n' "$max_resumes" >&2
fi

exit "$status"
