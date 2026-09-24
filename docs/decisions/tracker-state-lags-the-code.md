# The git log, not the tracker's state, says whether work is done

**Decision.** Before an agent is sent at a tracker item, whoever sends it checks the history for
that item and for the file or symbol it names. A tracker item's state is not evidence the work is
undone.

**Date.** 2026-09-20.

**Decided by.** Settled by the incident below.

**Why.** In one wave, two lanes were sent at items the tracker showed as in progress whose work had
already landed — one two weeks earlier — and a third item had been closed by a commit eight days
before. Each lane spent its whole run re-deriving that there was nothing to do. An item's state is
written by whoever remembers to write it: a completion that deliberately leaves the item open, or a
lane that finishes the code and not the tracker call, leaves it in progress for ever. The code is
the record of what is done; the tracker is the record of what is intended.

**How.** For each item, before the brief:

```sh
git log --oneline --all --grep <item-id>
git grep -n <the symbol or path the item names>
```

If the work is there, close the item and pick something else. If not, tell the lane to re-derive the
item's claims anyway: the ones still open are often open because their premise moved — a disk full
at 87% by one measure was 18% measured by content.

**Rules out.** Dispatching work on the tracker's state alone.

**Enforced at.** Nothing mechanical; the check is part of writing a brief.
