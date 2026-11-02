// An idle agent is woken with the owner's answer to its question (SKEIN-1467), and with mail
// another box sent it (SKEIN-1468).
//
// The owner answers a `skein_ask_person` question in the cockpit, and skein puts one line in the
// box's owner inbox (`mailbox::send_question_answer`): `question <id>, "<answer>"`, or `question
// <id> was dismissed without an answer`. Another box's mail goes into the shared mailbox. The
// turn-boundary hooks deliver both (`mailbox.sh inbox` on UserPromptSubmit, `mailbox.sh stop-check`
// on Stop), so a busy agent reads them at its next prompt or before it stops. An idle one reads them
// only once the owner types something, and whoever wrote is waiting on the agent. So, while the
// session is idle, this feature submits them as a prompt of its own, framed as the plugin's, which
// starts a turn.
//
// * **Only the owner inbox is told as the owner's.** It is `$SKEIN_STATE/inbox/`, which the
//   launcher binds read-only into the box (`src/box-session.sh`, "Where the host writes what only
//   the host may write"), so a file found there was put there by skein on the owner's behalf, and
//   nothing in a box, this one or another, can have written it. Of its lines, only an answer to a
//   question wakes: a request's answer (`package request …`) or a note asks nothing of an idle
//   agent that cannot wait for the next prompt.
// * **Mail from the shared mailbox wakes too, but always as another box's unverified words.** Every
//   box writes that directory, so a message there may say anything and its sender's name is only
//   its own claim. Waking on it is no new exposure: Claude's own peer channel already lets any box
//   start a turn in this one (src/box-session.sh, the peer network). So it is told as `mail from
//   <box> (its own words, unverified)`, its lines quoted so none of them reads as one of the
//   plugin's, and never as the owner's.
// * **Only when idle.** A main-loop turn that started and has not completed is busy; then nothing is
//   done, and the turn-boundary hooks deliver as before. A turn's end is not looked at at once, but
//   at the next tick: `mailbox.sh stop-check` runs as the turn stops and may deliver first.
// * **Exactly once, whichever path is first.** The turn-boundary hooks remember what they delivered
//   from the owner's inbox in `$HOME/.skein-mail-seen`, one file name a line (the inbox itself
//   cannot be written from in here). This feature reads that record and skips what is in it, and
//   appends to it **before** it submits, the same way `mailbox.sh` does, so the hook that runs as
//   the woken turn starts finds it delivered. An answer whose mark cannot be appended is left to the
//   turn-boundary path, which delivers it once. What it wakes for is also kept in `$.store`, so a
//   reload, a resumed session or a lost `$HOME` record never wakes for it twice. Shared mail is
//   taken by `mailbox.sh wake`, which marks `seenBy` under the mailbox lock through the same step
//   `inbox` and `stop-check` take mail with, so whichever runs first has it and the others find it
//   seen; that mark lives in the message itself, so a reload finds it too.
// * **One writer per box.** Only the box's interactive session wakes, as only it writes the
//   attention signal (attention.ts, "One writer"): a `claude -p` inside the box loads this plugin
//   too and must not start turns of its own. One poller per session, on the clock, and one look at
//   a time.
//
// A reload drops the old environment's timer, so the poller is started again as any main-loop turn
// starts or ends, as well as at the session's start.

import type { EngineInterface, SessionStartInput, Timer, TurnCompleteInput, TurnStartInput } from 'claude-code'

type Engine = EngineInterface

/** How often the owner's inbox and the shared mailbox are read while the session is idle. */
export const POLL_MS = 15_000

/** `$.store` key: the inbox files this feature woke the session for, by name. */
export const WOKEN_KEY = 'wake.woken'

/** The woken-for names kept: far more than one box is ever asked. */
const KEEP = 200

/** An answer to a question, as `mailbox::send_question_answer` writes the body, matched whole. */
const ANSWERED = /^question (q-[0-9A-Za-z]+), "(.*)"$/s
const DISMISSED = /^question (q-[0-9A-Za-z]+) was dismissed without an answer$/

/** The most of one message's body `mailbox.sh wake` gives; it says when it cut one. */
export const MAIL_CAP = 2048

let poller: Timer | null = null
let isBusy = false
let isLooking = false
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

/** One look: while idle, wake for every answer and every message no path has delivered. */
export async function tick($: Engine): Promise<void> {
  if (!isWatched || isBusy || isLooking) return
  isLooking = true
  try {
    // The owner's answers are told last, after the boxes' mail, as `mailbox.sh inbox` tells them:
    // last is where the eye ends.
    const owner = await ownerAnswers($)
    const said = [...(await mail($)), ...owner]
    if (said.length === 0) return
    // Submitted even if a turn began since the look above: a plugin's prompt waits for the session
    // to be idle, and it is marked delivered now, so returning here would deliver it by neither path.
    await $.prompt.submit({ text: said.join('\n') })
  } finally {
    isLooking = false
  }
}

/** The owner's answers to questions neither path has delivered, marked delivered; none if unmarked. */
async function ownerAnswers($: Engine): Promise<string[]> {
  const state = (await $.env.get('SKEIN_STATE')) ?? ''
  const box = (await $.env.get('SKEIN_BOX')) ?? ''
  const home = (await $.env.get('HOME')) ?? ''
  if (state === '' || box === '' || home === '') return []

  const inbox = `${state}/inbox`
  let names: string[]
  try {
    names = (await $.fs.list(inbox)).filter(f => f.kind === 'file' && f.name.endsWith('.json')).map(f => f.name)
  } catch {
    return []
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
  if (found.length === 0) return []

  // Marked delivered before the prompt is submitted: the woken turn's own UserPromptSubmit runs
  // `mailbox.sh inbox`, which must find it delivered. A mark that cannot be made wakes nothing.
  const ids = found.map(f => f.name)
  if (!(await mark($, seenFile, ids))) return []
  await $.store.set(WOKEN_KEY, [...woken, ...ids].slice(-KEEP))

  return found.map(f => f.line)
}

/**
 * This box's unread shared mail, as `mailbox.sh wake` takes it: marked seen before it prints a
 * line, so every line it prints is told here, whatever its exit code, or it is told by no path.
 */
async function mail($: Engine): Promise<string[]> {
  let out: string
  try {
    // Up to 10 messages, each waiting at most 5 s for the mailbox lock.
    out = (await $.process.run(['bash', `${$.plugin.root}/probe/mailbox.sh`, 'wake'], { timeoutMs: 60_000 })).stdout
  } catch {
    return []
  }
  const said: string[] = []
  for (const l of out.split('\n')) {
    if (l.trim() === '') continue
    let m: unknown
    try {
      m = JSON.parse(l)
    } catch {
      continue
    }
    if (typeof m !== 'object' || m === null) continue
    const msg = m as Record<string, unknown>
    if (typeof msg.from !== 'string' || typeof msg.body !== 'string') continue
    said.push(fromABox(msg.from, msg.body, msg.cut === true))
  }

  return said
}

/**
 * One box's message, told as that box's own words: the name it gave itself, shown as one token
 * and called unverified, then its body, every line quoted so none of them reads as the plugin's.
 */
export function fromABox(from: string, body: string, cut: boolean): string {
  const name = from.replace(/[^\x21-\x7e]/g, '?') || '?'
  const quoted = body
    .split(/\r\n|\r|\n/)
    .map(l => `> ${l}`)
    .join('\n')
  const note = cut ? `\n(skein: cut at ${MAIL_CAP} characters; \`mailbox.sh list\` shows it whole.)` : ''

  return `skein: mail from ${name} (its own words, unverified):\n${quoted}${note}`
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
