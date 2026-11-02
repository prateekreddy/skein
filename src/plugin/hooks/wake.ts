// An idle agent is woken with the owner's answer to its question (SKEIN-1467).
//
// The owner answers a `skein_ask_person` question in the cockpit, and skein puts one line in the
// box's owner inbox (`mailbox::send_question_answer`): `question <id>, "<answer>"`, or `question
// <id> was dismissed without an answer`. The turn-boundary hooks deliver it (`mailbox.sh inbox` on
// UserPromptSubmit, `mailbox.sh stop-check` on Stop), so a busy agent reads it at its next prompt or
// before it stops. An idle one reads it only once the owner types something, and the owner, who
// has just answered, is waiting on the agent. So, while the session is idle, this feature submits
// the answer as a prompt of its own, framed as the plugin's, which starts a turn.
//
// * **Only owner answers to questions wake a session.** They are read from `$SKEIN_STATE/inbox/`
//   and from nowhere else. The launcher binds that directory read-only into the box
//   (`src/box-session.sh`, "Where the host writes what only the host may write"), so a file found
//   there was put there by skein on the owner's behalf, and nothing in a box, this one or another,
//   can have written it. A wake is a turn the agent runs with nobody watching, on words it did not
//   choose: from the shared mailbox, which every box writes, it would let any box drive another,
//   and so box-to-box mail is never read here. It waits for the turn boundary, as it always did.
//   Of the owner's lines, only an answer to a question wakes: a request's answer (`package request
//   …`) or a note asks nothing of an idle agent that cannot wait for the next prompt.
// * **Only when idle.** A main-loop turn that started and has not completed is busy; then nothing is
//   done, and the turn-boundary hooks deliver as before. A turn's end is not looked at at once, but
//   at the next tick: `mailbox.sh stop-check` runs as the turn stops and may deliver it first.
// * **Exactly once, whichever path is first.** The turn-boundary hooks remember what they delivered
//   from the owner's inbox in `$HOME/.skein-mail-seen`, one file name a line (the inbox itself
//   cannot be written from in here). This feature reads that record and skips what is in it, and
//   appends to it **before** it submits, the same way `mailbox.sh` does, so the hook that runs as
//   the woken turn starts finds it delivered. An answer whose mark cannot be appended is left to the
//   turn-boundary path, which delivers it once. What it wakes for is also kept in `$.store`, so a
//   reload, a resumed session or a lost `$HOME` record never wakes for it twice.
// * **One writer per box.** Only the box's interactive session wakes, as only it writes the
//   attention signal (attention.ts, "One writer"): a `claude -p` inside the box loads this plugin
//   too and must not start turns of its own. One poller per session, on the clock.
//
// A reload drops the old environment's timer, so the poller is started again as any main-loop turn
// starts or ends, as well as at the session's start.

import type { EngineInterface, SessionStartInput, Timer, TurnCompleteInput, TurnStartInput } from 'claude-code'

type Engine = EngineInterface

/** How often the owner's inbox is read while the session is idle. */
export const POLL_MS = 15_000

/** `$.store` key: the inbox files this feature woke the session for, by name. */
export const WOKEN_KEY = 'wake.woken'

/** The woken-for names kept: far more than one box is ever asked. */
const KEEP = 200

/** An answer to a question, as `mailbox::send_question_answer` writes the body, matched whole. */
const ANSWERED = /^question (q-[0-9A-Za-z]+), "(.*)"$/s
const DISMISSED = /^question (q-[0-9A-Za-z]+) was dismissed without an answer$/

let poller: Timer | null = null
let isBusy = false
// Set at the session's start; a reload keeps the session it was, and `-p` never reloads.
let isWatched = false

// ------------------------------------------------------------------------------------------------
// The parts register.tsx calls
// ------------------------------------------------------------------------------------------------

export async function onStart($: Engine, e: SessionStartInput): Promise<void> {
  stop()
  isBusy = false
  isWatched = e.isInteractive
  start($)
}

export async function onTurnStart($: Engine, e: TurnStartInput): Promise<void> {
  if (e.agentId !== undefined) return
  isBusy = true
  start($)
}

export async function onTurnComplete($: Engine, e: TurnCompleteInput): Promise<void> {
  if (e.agentId !== undefined) return
  isBusy = false
  start($)
}

// ------------------------------------------------------------------------------------------------
// The poll
// ------------------------------------------------------------------------------------------------

function start($: Engine): void {
  if (!isWatched || poller !== null) return
  poller = $.clock.every(POLL_MS, () => {
    void tick($).catch(() => {})
  })
}

function stop(): void {
  poller?.cancel()
  poller = null
}

/** One look: while idle, wake for every answer neither path has delivered. */
export async function tick($: Engine): Promise<void> {
  if (!isWatched || isBusy) return
  const state = (await $.env.get('SKEIN_STATE')) ?? ''
  const box = (await $.env.get('SKEIN_BOX')) ?? ''
  const home = (await $.env.get('HOME')) ?? ''
  if (state === '' || box === '' || home === '') return

  const inbox = `${state}/inbox`
  let names: string[]
  try {
    names = (await $.fs.list(inbox)).filter(f => f.kind === 'file' && f.name.endsWith('.json')).map(f => f.name)
  } catch {
    return
  }
  names.sort()
  const seenFile = `${home}/.skein-mail-seen`
  const seen = new Set(await lines($, seenFile))
  const woken = new Set(await strings($))

  const found: { name: string; line: string }[] = []
  for (const name of names) {
    if (seen.has(name) || woken.has(name)) continue
    const line = await answer($, `${inbox}/${name}`, box)
    if (line !== null) found.push({ name, line })
  }
  if (found.length === 0) return

  // Marked delivered before the prompt is submitted: the woken turn's own UserPromptSubmit runs
  // `mailbox.sh inbox`, which must find it delivered. A mark that cannot be made wakes nothing.
  const ids = found.map(f => f.name)
  if (!(await mark($, seenFile, ids))) return
  await $.store.set(WOKEN_KEY, [...woken, ...ids].slice(-KEEP))
  // Submitted even if a turn began since the look above: a plugin's prompt waits for the session to
  // be idle, and it is marked delivered now, so returning here would deliver it by neither path.
  await $.prompt.submit({ text: found.map(f => f.line).join('\n') })
}

/** What the agent is told of one inbox file, or null when it is no answer to a question for it. */
async function answer($: Engine, path: string, box: string): Promise<string | null> {
  let m: unknown
  try {
    m = JSON.parse(await $.fs.read(path))
  } catch {
    return null
  }
  if (typeof m !== 'object' || m === null) return null
  const msg = m as Record<string, unknown>
  if (msg.from !== 'skein' || msg.kind !== 'answer' || msg.to !== box || typeof msg.body !== 'string') return null
  const said = ANSWERED.exec(msg.body)
  if (said !== null) return `skein: the owner answered your question ${said[1]}: "${said[2]}".`
  const dismissed = DISMISSED.exec(msg.body)
  if (dismissed !== null) return `skein: the owner dismissed your question ${dismissed[1]} without an answer.`

  return null
}

/** The lines of a file; none when it cannot be read. */
async function lines($: Engine, path: string): Promise<string[]> {
  try {
    return (await $.fs.read(path)).split('\n').map(l => l.trim()).filter(l => l !== '')
  } catch {
    return []
  }
}

/**
 * Appends each name to the turn-boundary hooks' record, as `mailbox.sh` itself does (`>>`, never
 * a rewrite, so a line it appends meanwhile is not lost). False when the append failed.
 */
async function mark($: Engine, seenFile: string, ids: string[]): Promise<boolean> {
  try {
    const ran = await $.process.run(['sh', '-c', 'f="$1"; shift; printf "%s\\n" "$@" >> "$f"', 'skein-wake', seenFile, ...ids], {
      timeoutMs: 5_000,
    })

    return ran.exitCode === 0
  } catch {
    return false
  }
}

async function strings($: Engine): Promise<string[]> {
  const v = await $.store.get(WOKEN_KEY)

  return Array.isArray(v) ? v.filter((s): s is string => typeof s === 'string') : []
}
