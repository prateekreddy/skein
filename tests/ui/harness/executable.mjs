// Every executable a UI suite writes into its fixture, written one way (SKEIN-1465).
//
// **A file a process is executing cannot be opened for writing.**
// `hatches.mjs` failed CI run 41318757630 at "the harness suite could run at all" with `ETXTBSY:
// text file is busy, open '…/server-home/.local/bin/claude'`. `startServer` writes the box-agent
// stand-ins into the fixture's home every time it starts a server, and that suite starts a dozen
// servers on one fixture, so each start rewrote a file the previous one's processes could still be
// running. The same failure was filed once before, as SKEIN-1407, and it was never fixed: the item
// went to the backlog with "written to a temp name and renamed into place" as its suggested fix, and
// no commit cites it. So it came back on the same path.
//
// So nothing writes over an executable. This writes a new file beside the target and `rename()`s it
// over the target. A rename never opens the old file. A process already running the old file keeps
// its own inode, and the next exec gets the new one whole, never half written. The temporary is in
// the target's own directory because a rename across filesystems is a copy.
//
// The check that holds this is `hatches.mjs` check 8. It asks for the refusal not to happen, and it
// also asks for the property the refusal protects: after the write, the path is a different inode
// from the one the process is running. Linux has more than once tried dropping the write denial on
// running executables, and a kernel without it would let an in-place write pass the first question
// and fail the second.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

/** Put `content` at `file` as an executable (0755), by rename, never by writing over it. */
export function writeExecutable(file, content) {
  const tmp = path.join(
    path.dirname(file),
    `.${path.basename(file)}.${process.pid}-${crypto.randomBytes(4).toString("hex")}.tmp`,
  );
  try {
    fs.writeFileSync(tmp, content, { mode: 0o755, flag: "wx" });
    // The mode above is masked by the umask, so it is set again.
    fs.chmodSync(tmp, 0o755);
    fs.renameSync(tmp, file);
  } catch (e) {
    fs.rmSync(tmp, { force: true });
    throw e;
  }
  return file;
}
