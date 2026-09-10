#!/bin/sh
set -u

review_dir=${A3S_CODEX_STOP_REVIEW_DIR:-/tmp/a3s-codex-stop-review}
count_file="$review_dir/convergence-count"
accepted_file="$review_dir/accepted"
limit=10

payload=$(cat)
normalized=$(printf '%s' "$payload" | tr '[:upper:]' '[:lower:]')

converged=0
case "$normalized" in
    *"no further "*"change"* | *"no further "*"edit"* | *"no further "*"modification"* | \
        *"no further "*"improvement"* | *"no further "*"optimization"* | \
        *"no additional "*"change"* | *"no additional "*"edit"* | \
        *"nothing further to "* | *"nothing else to "* | \
        *"cannot identify any further"* | *"can't identify any further"* | \
        *"already good enough"* | *"is good enough"* | *"work is complete"* | \
        *"already complete"* | *"already optimal"* | *"current final candidate"* | \
        *"will not make further"* | *"won't make further"* | *"decline to make further"* | \
        *"无需进一步"* | *"不需要进一步"* | *"没有更多"*"修改"* | \
        *"没有更多"*"改动"* | *"没有更多"*"优化"* | *"已经足够"* | \
        *"已足够"* | *"已经完成"* | *"不再修改"* | *"拒绝继续"* | *"拒绝再"*)
        converged=1
        ;;
esac

if ! mkdir -p "$review_dir" 2>/dev/null; then
    echo '{"decision":"block","reason":"Review the implementation again and make a concrete, validated improvement if one remains."}'
    exit 0
fi

if [ "$converged" -eq 0 ]; then
    rm -f "$count_file"
    echo '{"decision":"block","reason":"Review the implementation again. Make a concrete, validated improvement if one remains; if none remains, explicitly say that no further meaningful changes are warranted."}'
    exit 0
fi

count=0
if [ -r "$count_file" ]; then
    IFS= read -r count <"$count_file" || count=0
fi
case "$count" in
    '' | *[!0-9]*) count=0 ;;
esac
count=$((count + 1))

if [ "$count" -ge "$limit" ]; then
    if : >"$accepted_file"; then
        rm -f "$count_file"
        echo '{}'
        exit 0
    fi
    count=$((limit - 1))
fi

count_tmp="$count_file.$$"
if printf '%s\n' "$count" >"$count_tmp" && mv "$count_tmp" "$count_file"; then
    printf '{"decision":"block","reason":"Convergence claim %d/%d noted. Continue only with a concrete, validated improvement; otherwise restate why no further meaningful change is warranted."}\n' "$count" "$limit"
else
    rm -f "$count_tmp"
    echo '{"decision":"block","reason":"Review the implementation again and make a concrete, validated improvement if one remains."}'
fi
