import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";

test("release installer verifies all four commands and fails closed without fallback", { skip: process.platform === "win32" }, (t) => {
  const dir = mkdtempSync(join(tmpdir(), "acs-install-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const shim = join(dir, "shim");
  const payload = join(dir, "payload");
  mkdirSync(shim);
  mkdirSync(payload);
  for (const name of ["acs", "aos", "qagent", "acs-desktop"]) {
    writeFileSync(join(payload, name), "#!/bin/sh\nprintf 'fixture 1.0.0\\n'\n");
  }
  const archive = join(dir, "archive.tar.gz");
  assert.equal(spawnSync("tar", ["-czf", archive, "-C", payload, "acs", "aos", "qagent", "acs-desktop"]).status, 0);
  const hash = createHash("sha256").update(readFileSync(archive)).digest("hex");
  writeFileSync(join(dir, "checksum"), `${hash}  archive.tar.gz\n`);
  const symlinkPayload = join(dir, "symlink-payload");
  mkdirSync(symlinkPayload);
  for (const name of ["acs", "aos", "qagent", "acs-desktop"]) {
    if (name === "aos") symlinkSync("acs", join(symlinkPayload, name));
    else writeFileSync(join(symlinkPayload, name), "#!/bin/sh\nprintf 'fixture 1.0.0\\n'\n");
  }
  const symlinkArchive = join(dir, "symlink-archive.tar.gz");
  assert.equal(spawnSync("tar", ["-czf", symlinkArchive, "-C", symlinkPayload, "acs", "aos", "qagent", "acs-desktop"]).status, 0);
  const symlinkHash = createHash("sha256").update(readFileSync(symlinkArchive)).digest("hex");
  writeFileSync(join(dir, "symlink-checksum"), `${symlinkHash}  archive.tar.gz\n`);
  const curl = join(shim, "curl");
  writeFileSync(curl, `#!/bin/sh
url=; output=
while [ "$#" -gt 0 ]; do
  case "$1" in https://*) url=$1 ;; -o) shift; output=$1 ;; esac
  shift
done
case "$url" in
  */releases/latest) printf '{"tag_name":"v1.0.0"}\\n' > "$output" ;;
  *.sha256)
    [ "$INSTALL_CASE" != missing ] || exit 22
    if [ "$INSTALL_CASE" = symlink ]; then cp "$FIXTURE/symlink-checksum" "$output"
    else cp "$FIXTURE/checksum" "$output"; fi ;;
  *.tar.gz)
    [ "$INSTALL_CASE" != absent ] || exit 22
    if [ "$INSTALL_CASE" = symlink ]; then cp "$FIXTURE/symlink-archive.tar.gz" "$output"
    else cp "$FIXTURE/archive.tar.gz" "$output"; fi ;;
  *) exit 23 ;;
esac
`);
  chmodSync(curl, 0o755);
  for (const name of ["git", "cargo"]) {
    writeFileSync(join(shim, name), "#!/bin/sh\ntouch \"$FIXTURE/source-invoked\"\nexit 1\n");
    chmodSync(join(shim, name), 0o755);
  }
  const run = (kind: string, destination = join(dir, kind)) => spawnSync("sh", [resolve("install.sh")], {
    encoding: "utf8",
    env: { ...process.env, PATH: `${shim}:${process.env.PATH}`, FIXTURE: dir, INSTALL_CASE: kind,
      ACS_INSTALL_DIR: destination, ACS_VERSION: "", ACS_FROM_SOURCE: "0" },
  });
  const success = run("valid");
  assert.equal(success.status, 0, success.stderr);
  assert.match(success.stdout, /ACS release: v1.0.0/);
  assert.match(success.stdout, /SHA-256 verified/);
  for (const name of ["acs", "aos", "qagent", "acs-desktop"]) {
    assert.equal(spawnSync(join(dir, "valid", name), ["--version"], { encoding: "utf8" }).stdout.trim(), "fixture 1.0.0");
  }
  for (const kind of ["missing", "absent"]) {
    const result = run(kind);
    assert.notEqual(result.status, 0);
    assert.equal(existsSync(join(dir, kind)), false);
  }
  const symlink = run("symlink");
  assert.notEqual(symlink.status, 0);
  assert.match(symlink.stdout, /SHA-256 verified/);
  assert.match(symlink.stderr, /missing regular binary: aos/);
  assert.equal(existsSync(join(dir, "symlink")), false);
  const existingDestination = join(dir, "symlink-existing");
  mkdirSync(existingDestination);
  writeFileSync(join(existingDestination, "keep.txt"), "preserve");
  const existingSymlink = run("symlink", existingDestination);
  assert.notEqual(existingSymlink.status, 0);
  assert.equal(readFileSync(join(existingDestination, "keep.txt"), "utf8"), "preserve");
  assert.equal(existsSync(join(existingDestination, "acs")), false);
  writeFileSync(join(dir, "checksum"), `${"0".repeat(64)}  archive.tar.gz\n`);
  const corrupt = run("corrupt");
  assert.notEqual(corrupt.status, 0);
  assert.match(corrupt.stderr, /checksum mismatch/);
  assert.equal(existsSync(join(dir, "corrupt")), false);
  assert.equal(existsSync(join(dir, "source-invoked")), false);
});
