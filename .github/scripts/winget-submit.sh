#!/usr/bin/env bash
# Submits one xmux version's winget manifests to microsoft/winget-pkgs through
# the zer0ken/winget-pkgs fork, and answers the Microsoft CLA bot.
#
# Shared by release.yml (at release time) and winget-sync.yml (the daily
# backstop), so a fix to the submission lands in both at once.
#
#   VERSION            the version to submit, e.g. 0.9.19
#   MANIFEST_DIR       a directory holding the three zer0ken.xmux*.yaml files
#   WINGET_PKGS_TOKEN  a classic PAT with `public_repo`. A fine-grained token
#                      can write the fork but is refused opening the PR on
#                      microsoft/winget-pkgs, which it cannot be granted.
set -euo pipefail
: "${VERSION:?}" "${MANIFEST_DIR:?}" "${WINGET_PKGS_TOKEN:?}"

API=https://api.github.com
FORK=zer0ken/winget-pkgs
UPSTREAM=microsoft/winget-pkgs
OWNER=${FORK%%/*}
BR="add/zer0ken.xmux.v${VERSION}"
AUTH="Authorization: Bearer $WINGET_PKGS_TOKEN"
FILES="zer0ken.xmux.yaml zer0ken.xmux.installer.yaml zer0ken.xmux.locale.en-US.yaml"
BOT=microsoft-github-policy-service[bot]

# Every call fails on an HTTP error, and a refusal's body goes to stderr:
# callers discard stdout, and GitHub's message is the only place the reason is.
api() {
  local out
  if ! out=$(curl --fail-with-body -sS -H "$AUTH" -H "Accept: application/vnd.github+json" "$@"); then
    printf '%s\n' "$out" >&2
    return 1
  fi
  printf '%s' "$out"
}

open_pr() {
  api "$API/repos/$UPSTREAM/pulls?state=open&head=$OWNER:$BR" | jq -r '.[0].number // empty'
}

PR=$(open_pr)
if [ -n "$PR" ]; then
  # Resetting the branch under an open PR empties it, and GitHub closes a PR
  # with no changes - which is how a re-run used to close its own submission.
  echo "winget-pkgs PR #$PR is already open for $BR; leaving its branch alone."
else
  # Base the branch on the newest commit the fork and upstream share.
  # Upstream's newer commits change .github/workflows, and a ref that brings
  # them into the fork is refused to a token without Workflows write; the
  # fork's own master holds commits upstream never took, which would ride into
  # the PR. The merge base is in both, so the branch adds exactly the manifests.
  BASE=$(api "$API/repos/$UPSTREAM/compare/master...$OWNER:${FORK#*/}:master" \
    | jq -er .merge_base_commit.sha)
  # The single-ref lookup takes `heads/<branch>`, not `refs/heads/<branch>`.
  if api "$API/repos/$FORK/git/ref/heads/$BR" >/dev/null 2>&1; then
    api -X PATCH "$API/repos/$FORK/git/refs/heads/$BR" \
      -d "{\"sha\":\"$BASE\",\"force\":true}" >/dev/null
  else
    api -X POST "$API/repos/$FORK/git/refs" \
      -d "{\"ref\":\"refs/heads/$BR\",\"sha\":\"$BASE\"}" >/dev/null
  fi

  for f in $FILES; do
    jq -n --arg b64 "$(base64 -w0 "$MANIFEST_DIR/$f")" --arg msg "Add zer0ken.xmux $VERSION manifest" \
      --arg br "$BR" '{message:$msg, content:$b64, branch:$br}' > /tmp/winget-body.json
    api -X PUT "$API/repos/$FORK/contents/manifests/z/zer0ken/xmux/$VERSION/$f" \
      --data-binary @/tmp/winget-body.json >/dev/null
  done

  jq -n --arg head "$OWNER:$BR" --arg title "New version: zer0ken.xmux version $VERSION" \
    --arg body "Updates the winget manifest to zer0ken.xmux $VERSION. Generated automatically by the xmux release workflow." \
    '{title:$title, head:$head, base:"master", body:$body}' > /tmp/winget-pr.json
  PR=$(api -X POST "$API/repos/$UPSTREAM/pulls" --data-binary @/tmp/winget-pr.json | jq -er .number)
  echo "Opened https://github.com/$UPSTREAM/pull/$PR"
fi

# The CLA bot reads an `agree` only as a reply: one posted before it asks is
# ignored and the PR stays Needs-CLA. So wait for it to either ask (label
# Needs-CLA) or show it will not (its validation badge, then a quiet spell),
# and agree only after it asked.
labels() { api "$API/repos/$UPSTREAM/issues/$PR/labels" | jq -r '.[].name'; }
comments() { api "$API/repos/$UPSTREAM/issues/$PR/comments?per_page=100"; }
ME=$(api "$API/user" | jq -er .login)
# Whether the bot asked and an agreement from us came after its last ask.
answered() {
  comments | jq -e --arg bot "$BOT" --arg me "$ME" '
    (map(select(.user.login == $bot and (.body | test("needsCLA")))) | last | .created_at) as $ask
    | $ask != null
      and any(.[]; .user.login == $me and (.body | test("@microsoft-github-policy-service agree"))
                   and .created_at > $ask)' >/dev/null
}

deadline=$((SECONDS + 900))
badge_at=""
asked=""
while [ $SECONDS -lt $deadline ]; do
  if labels | grep -qx 'Needs-CLA'; then asked=1; break; fi
  # Asked and answered on an earlier run: nothing left to wait for.
  if answered; then echo "CLA already accepted on PR #$PR."; exit 0; fi
  if [ -z "$badge_at" ] && comments | jq -e --arg bot "$BOT" \
      'any(.[]; .user.login == $bot and (.body | test("Validation Pipeline Badge")))' >/dev/null; then
    badge_at=$SECONDS
  fi
  # The bot has always asked within two minutes of its badge; five without
  # asking means this author's CLA is already on record.
  if [ -n "$badge_at" ] && [ $((SECONDS - badge_at)) -ge 300 ]; then break; fi
  sleep 20
done

if [ -z "$asked" ]; then
  if [ -n "$badge_at" ]; then
    echo "The CLA bot did not ask; nothing to agree to."
    exit 0
  fi
  echo "::error::The winget-pkgs bots did not respond on PR #$PR within 15 minutes; answer the CLA there by hand if it asks."
  exit 1
fi

# Agree once per ask: an agreement already posted after the bot's last
# request is the answer, and posting another is noise on someone's queue.
if ! answered; then
  api -X POST "$API/repos/$UPSTREAM/issues/$PR/comments" \
    -d '{"body":"@microsoft-github-policy-service agree"}' >/dev/null
  echo "Agreed to the CLA on PR #$PR."
fi

for _ in $(seq 1 15); do
  if ! labels | grep -qx 'Needs-CLA'; then
    echo "CLA accepted on PR #$PR."
    exit 0
  fi
  sleep 20
done
echo "::error::PR #$PR is still Needs-CLA five minutes after agreeing; check it by hand."
exit 1
