#!/usr/bin/env bash
# Finish a release that .github/workflows/release.yml left as a draft:
# download its SHA256SUMS, add SHA256SUMS.sig (the in-app updater's ed25519
# signature), check it against the key built into the app, check the
# installer's code signature, optionally set the release notes, publish.
#
#   scripts/finish-release.sh v0.8.0 [notes.md]
#
# Signing: with the release key on this machine ($RELEASE_KEY, default
# ~/.config/page2cam/release-signing.key) it signs here; otherwise set
# SIGN_HOST=user@host, a machine reachable by ssh key that holds the key at
# that same path, and the signing happens there (the key never leaves it).
# Needs: gh (logged in), openssl, ssh/scp for SIGN_HOST.
set -euo pipefail

TAG="${1:?usage: $0 <tag, e.g. v0.8.0> [notes.md]}"
NOTES="${2:-}"
REPO="${REPO:-prasadvittaldev/Page2Cam}"
RELEASE_KEY="${RELEASE_KEY:-$HOME/.config/page2cam/release-signing.key}"
# The public half, as in crates/p2c-update (RELEASE_PUBLIC_KEY).
PUBLIC_KEY_B64="yY3GQs5sx8xIMDKQZ8Nc18IlL/oTfE+3mAdoMGF/ZZM="

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

[ "$(gh release view "$TAG" -R "$REPO" --json isDraft --jq .isDraft)" = true ] ||
  { echo "$TAG is not a draft release (already published?)" >&2; exit 1; }

echo "==> Downloading $TAG"
gh release download "$TAG" -R "$REPO" -p 'SHA256SUMS' -p 'Page2Cam-Setup-*.exe' -p 'Page2Cam-windows-x64.zip'
sha256sum -c SHA256SUMS

echo "==> Signing SHA256SUMS"
# ed25519 private key DER = PKCS#8 prefix + the 32-byte seed (last 32 bytes of the key file).
sign_cmd='set -e; t=$(mktemp -d); trap "rm -rf $t" EXIT; seed=$(base64 -d "$1" | od -An -tx1 | tr -d " \n" | tail -c 64); printf "302e020100300506032b657004220420%s" "$seed" | xxd -r -p > $t/k.der; openssl pkey -inform DER -in $t/k.der -out $t/k.pem; openssl pkeyutl -sign -inkey $t/k.pem -rawin -in "$2" -out "$3"'
if [ -f "$RELEASE_KEY" ]; then
  bash -c "$sign_cmd" _ "$RELEASE_KEY" SHA256SUMS SHA256SUMS.sig
elif [ -n "${SIGN_HOST:-}" ]; then
  remote="/tmp/p2c-sums-$$"
  scp -q -o BatchMode=yes SHA256SUMS "$SIGN_HOST:$remote"
  ssh -o BatchMode=yes "$SIGN_HOST" "bash -c '$sign_cmd' _ ~/.config/page2cam/release-signing.key $remote $remote.sig; base64 -w0 $remote.sig; rm -f $remote $remote.sig" |
    base64 -d > SHA256SUMS.sig
else
  echo "no release key at $RELEASE_KEY and no SIGN_HOST set" >&2; exit 1
fi

echo "==> Checking"
printf '302a300506032b6570032100%s' "$(echo "$PUBLIC_KEY_B64" | base64 -d | od -An -tx1 | tr -d ' \n')" | xxd -r -p > pub.der
openssl pkeyutl -verify -pubin -keyform DER -inkey pub.der -rawin -in SHA256SUMS -sigfile SHA256SUMS.sig
setup="$(ls Page2Cam-Setup-*.exe)"
if command -v powershell >/dev/null; then
  powershell -NoProfile -Command "\$s = Get-AuthenticodeSignature '$setup'; \"code signature: \$(\$s.Status) \$(\$s.SignerCertificate.Subject)\""
fi

echo "==> Publishing $TAG"
gh release upload "$TAG" -R "$REPO" SHA256SUMS.sig --clobber
args=(--draft=false --latest)
[ -n "$NOTES" ] && args+=(--notes-file "$NOTES")
gh release edit "$TAG" -R "$REPO" "${args[@]}"
gh release view "$TAG" -R "$REPO" --json url,assets --jq '.url, (.assets[] | "  " + .name + " " + (.size|tostring))'
