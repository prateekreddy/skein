# What only a live fleet can answer

Everything below is checked by tests **except the part that needs a real sandbox**. This machine has
no `sbx`, so eight landings of 2026-08-25 are proven against the fake-sbx harness and against real
`bwrap` where the question was a mount — and the residue is this page: a short list of things the
owner can run once, after a rebuild, that say whether the fleet agrees.

Each entry names the command, what a pass looks like, and — the part worth writing down — **what a
failure means**, because half of these fail in a way that looks like something else.

Run them in order. The first is the only one the rest depend on.

---

## 0. The build under test

```
skein doctor | head -1
```

Every line below is a claim made by *some* build, and twice they have been pinned on the wrong one.
Expect a revision at or after `9c3d7bd`. A `-dirty` suffix is fine — it means built from a working
tree, not that anything is wrong.

While you are there, two lines in that output are new: `deployment` (where skein is running, and what
that implies) and `fleet disk` (below).

---

## 1. The cockpit's door — opened before any box exists

Inside the sandbox:

```
ss -ltnp | grep :7878
```

**Pass:** the listener names `python3 …/.skein/server-doorway.py`, and it is there *before any
server has been built inside the sandbox* — a fleet that has only been created should already show
it.

**Failure means:** the port stood free for some interval after create, and in that interval the first
box to bind it becomes the cockpit — the browser hands it the fleet token on the first request
(architecture §9.4). This is the one failure on the page that is a security failure rather than an
inconvenience.

### 1a. An upgrade keeps the same socket

With a cockpit tab open, re-run `bootstrap.sh` in the sandbox (or send the doorway `-USR1`), then
inside the sandbox:

```
pgrep -f server-doorway.py
readlink /proc/<pid>/fd/3
```

**Pass:** the pid is unchanged (a reload is an `exec`, so the pid survives on purpose), the descriptor
names the *same* socket inode, and the tab reconnects on the same URL without being reloaded.

**Failure means:** the upgrade closed and re-bound the port. Same exposure as §1, in a window rather
than an interval.

### 1b. The door comes back by itself

```
kill -9 <doorway pid>          # inside the sandbox
```

**Pass:** from the host, the published port refuses for well under a second and then answers again.

**Failure means:** one of two things, and they are distinguishable. If a `skein-server` is still
running and holding :7878 with no doorway above it, `PR_SET_PDEATHSIG` did not take — this sandbox
image does not honour it, and nothing will ever re-bind until that server is killed by hand. If
nothing holds the port for two seconds or more, the supervisor's conditional delay is not conditional
in this shell.

### 1c. A squatter is refused rather than published to

Bind :7878 from inside a box, then create a fleet — or call `fleet::cockpit_port_advice` — and read
what skein tells you to run.

**Pass:** it refuses, names §9.4, and does **not** print an `sbx ports … --publish` line. Skein no
longer publishes anything itself (SKEIN-576), so what is under test is the advice: the mapping is
yours to make, and this is the check that skein never asks you to make it onto a squatter.

**Failure means:** the publish was judged by a TCP connect. A squatter accepts a connect exactly as
the doorway does, and the mapping it would be given is not skein's to take back: `sbx ports
<sandbox> --unpublish HOST:SANDBOX` exists, and skein has no privileged path to call it — the warden
carries `create` and `destroy` and nothing else. Undoing it is a line a person runs.

---

## 2. The cover over the volume a serving fleet mounts

From inside **any ordinary box** (not the workshop box, which is exempt by design):

```
cat "$SKEIN_HOME/api-token"     ; echo "exit $?"
ls  "$SKEIN_HOME/credentials"   ; echo "exit $?"
ls  "$SKEIN_HOME/github-pats"   ; echo "exit $?"
```

**Pass:** all three fail — no such file or directory. The box still has its own store, its own state,
its own checkout and the git token the host placed for it; check one of those in the same breath, so
a pass is "covered" rather than "broke everything".

**Failure means:** this fleet was created by a skein that predates the ancestor cover, or the launcher
in the sandbox is stale. A box that can read `credentials/` **is** the fleet: it can act as any box,
push anywhere the fleet can push, and read every other box's conversation. Recreate the fleet, or at
minimum restart every box so each takes the current launcher.

---

## 3. The disk, and which of the two is full

```
skein doctor | grep -A1 "fleet disk"
```

**Pass:** it names both filesystems with their percentages. Past 85% it becomes a fault that names
what to clear — the largest boxes for one, `docker system prune -af` for the other, and those are
different actions on different filesystems.

Then confirm the figure is the truth rather than a cached guess, inside the sandbox:

```
du -sxm /boxes/*/ | sort -rn | head
df -Pm /
```

**Failure means:** if `doctor`'s figure and `du`'s disagree by more than a box's churn, the in-fleet
disk walk is measuring something other than what `du` measures — sparse files, hardlinks and
symlinks out of the tree are each a way to be wrong here, and each was checked against real `du` in
the harness.

---

## 4. The warden's record is off the volume

On the host:

```
ls ~/.skein-warden/                 # audit.jsonl, outcomes/
ls "${SKEIN_HOME:-$HOME/.skein}/warden/"   # secret, and nothing else
```

**Pass:** the log and the outcomes are in the first, the secret alone in the second. If the fleet ran
an older warden, the first start after this lands *moves* what it left behind, and says so on stderr.

**Failure means:** the record is on the volume that is mounted into the fleet, where the thing being
audited can rewrite it — and an outcome written there is an answer the warden would serve as its own.

Note the pairing while you are here: skein and the warden read their own environments. If you point
one at a volume with `SKEIN_HOME` and not the other, every request is refused for a mismatched
secret, which reads as a broken warden rather than as a disagreement. The warden now prints which
volume it is paired to at start.

---

## 5. Turn state is attributed to the box it came from

For a box that is running, in its store:

```
cat <store>/status/<box>.pane.json | head -c 200
```

**Pass:** the JSON carries `"box":"<that box's name>"`.

You will also find `skein-fleet.pane.json` in some stores — the *sandbox's* name, from before this
landed. Those are inert: the board removes the fleet sandbox from the box list, so nothing ever reads
them. Deleting them is safe and optional.

**Failure means:** the probe in that store predates the fix. It is not a fault — an observation with
no `box` is accepted deliberately, so that a box still running an older probe does not go dark — but
it is not checked either, and the row's badge will not say `misfiled` if it ever is.

---

## 6. The review surface, before you trust it with a merge

The merge train is **off** until you turn it on (`pr_workflows` in settings), and that is the point of
this check: with it off, the workflows pane already shows what the train *would* do — the front pull
request, the step it is on, everyone behind it, and anything stopped with the reason. That is a dry
run you can read before anything acts.

**Pass:** the panel names a front PR and a step you agree with, and the banner row names any repo with
skipped PRs. Then, and only then, turn it on.

**Worth knowing while you read it:** the reading budget is 100 pull requests a day of skein's *own*
initiative, fleet-wide. Anything you press yourself — read, re-read, draft again — is never counted
and never blocked. When the automatic budget is spent, the row says so and invites you to press it.
