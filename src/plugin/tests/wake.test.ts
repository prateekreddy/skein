// An idle agent woken with the owner's answer to its question (SKEIN-1467), through the whole
// module.
//
// Nothing sits beneath the plugin under test: the box's owner inbox, the shared mailbox, the
// turn-boundary hooks' delivered record (`$HOME/.skein-mail-seen`) and the prompt the session runs
// are this file's own, and `sh` appending to the record is answered by appending to it here.

import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'
import type { MockClock } from 'claude-code/testing'

const NOW = Date.parse('2026-11-02T12:00:00Z')
const BOX = 'web-main'
const HOME = '/home/agent'
const STATE = '/run/skein/state/web-main'
const INBOX = `${STATE}/inbox`
const SEEN = `${HOME}/.skein-mail-seen`
const MAILBOX = '/boxes/.skein/store/mailbox'
const START = { cwd: '/boxes/web-main/tree', surface: 'terminal', isInteractive: true } as const
const TURN_END = { answer: '', durationMs: 5, isAborted: false, turnId: 't1', reason: 'answer' } as const
const TICK = 15_000

type Box = {
  clock: MockClock
  /** Files by absolute path. */
  files: Map<string, string>
  /** Every prompt the plugin submitted, as text. */
  submitted: string[]
  /** Every directory the plugin listed. */
  listed: string[]
}

/** A message as `mailbox::send_message` writes it into an owner inbox. */
const owner = (body: string, to = BOX) =>
  JSON.stringify({ from: 'skein', to, kind: 'answer', branch: '', body, ts: '2026-11-02T12:00:00Z', seenBy: [] })

function engine(on: On, store: Record<string, unknown> = {}): Box {
  const b: Box = { clock: mock.clock(on, { now: NOW }), files: new Map(), submitted: [], listed: [] }
  mock.env(on, { HOME, SKEIN_BOX: BOX, SKEIN_STATE: STATE })
  mock.store(on, store)
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('session.id', () => ({ value: 'session-1' }))
  on('turn.start', (_, e) => ({ turnId: e.turnId }))
  on('turn.complete', (_, e) => ({ text: e.answer }))
  on('command.register', (_, e) => ({ value: { command: e.name } }))
  on('tool.register', (_, e) => ({ value: { tool: `mcp__skein__${e.name}` } }))
  on('fs.read', (_, e) => {
    const got = b.files.get(e.path)
    if (got === undefined) throw new Error(`ENOENT: ${e.path}`)

    return { value: got }
  })
  on('fs.write', (_, e) => {
    b.files.set(e.path, e.text)

    return { value: undefined }
  })
  on('fs.list', (_, e) => {
    b.listed.push(e.path)
    const names = [...b.files.keys()].filter(p => p.startsWith(`${e.path}/`)).map(p => p.slice(e.path.length + 1))
    if (names.length === 0) throw new Error(`ENOENT: ${e.path}`)

    return { value: names.map(name => ({ name, kind: 'file' as const, size: 100, mtimeMs: NOW, isLink: false })) }
  })
  on('process.run', (_, e) => {
    const argv = [...e.argv]
    const ok = argv[0] === 'sh' && argv[3] === 'skein-wake'
    if (ok) b.files.set(argv[4] ?? '', (b.files.get(argv[4] ?? '') ?? '') + argv.slice(5).map(id => `${id}\n`).join(''))

    return { value: { exitCode: ok ? 0 : 1, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
  })
  on('prompt.submit', (_, e) => {
    b.submitted.push(e.text)

    return { text: e.text }
  })

  return b
}

// What would make it fail: an idle session that waits for the owner to type, the answer submitted
// in other words or not at all, or submitted again at the next tick because nothing remembered it.
// Watched: with `tick` returning before `$.prompt.submit`, `submitted` was [] and the first
// comparison failed; with the `seen.has(name) || woken.has(name)` skip made always false, the
// second tick submitted it again and the last comparison read two prompts.
test('an idle session is woken once with the owner answer, and not again at the next tick', async ($, on) => {
  const b = engine(on)
  await $.session.start(START)
  b.files.set(`${INBOX}/100-skein.json`, owner('question q-12ab34cd, "use the new table"'))

  await b.clock.advance(TICK)
  expect(b.submitted).toEqual(['skein: the owner answered your question q-12ab34cd: "use the new table".'])
  // Marked in the turn-boundary hooks' own record, so `mailbox.sh inbox` does not deliver it again.
  expect(b.files.get(SEEN)).toBe('100-skein.json\n')

  await b.clock.advance(TICK)
  expect(b.submitted.length).toBe(1)
})

// What would make it fail: a dismissal that wakes nothing, or is told as an answer.
// Watched: with the DISMISSED branch in `answer` removed, `submitted` was [].
test('a dismissal wakes the session too, in its own words', async ($, on) => {
  const b = engine(on)
  await $.session.start(START)
  b.files.set(`${INBOX}/100-skein.json`, owner('question q-12ab34cd was dismissed without an answer'))

  await b.clock.advance(TICK)
  expect(b.submitted).toEqual(['skein: the owner dismissed your question q-12ab34cd without an answer.'])
})

// What would make it fail: a busy session woken mid-turn, or one never woken once idle.
// Watched: with `isBusy` never set in `onTurnStart`, the first comparison read the prompt
// submitted during the turn; with `onTurnComplete` leaving `isBusy` true, the last read [].
test('a busy session is left alone, and woken once it is idle', async ($, on) => {
  const b = engine(on)
  await $.session.start(START)
  await $.turn.start({ text: 'work on it', turnId: 't1' })
  b.files.set(`${INBOX}/100-skein.json`, owner('question q-12ab34cd, "yes"'))

  await b.clock.advance(TICK * 3)
  expect(b.submitted).toEqual([])

  await $.turn.complete(TURN_END)
  await b.clock.advance(TICK)
  expect(b.submitted).toEqual(['skein: the owner answered your question q-12ab34cd: "yes".'])
})

// What would make it fail: the wake ignoring what the turn-boundary hooks already delivered, so the
// agent reads one answer twice. Watched: with `seen.has(name)` dropped from the skip in `tick`, the
// session was woken with an answer `mailbox.sh stop-check` had already delivered.
test('an answer the turn-boundary hooks delivered while the session was busy wakes nothing', async ($, on) => {
  const b = engine(on)
  await $.session.start(START)
  await $.turn.start({ text: 'work on it', turnId: 't1' })
  b.files.set(`${INBOX}/100-skein.json`, owner('question q-12ab34cd, "yes"'))
  await b.clock.advance(TICK)
  // `mailbox.sh stop-check` delivers it as the turn stops, and records it as `mailbox.sh` does.
  b.files.set(SEEN, '100-skein.json\n')
  await $.turn.complete(TURN_END)

  await b.clock.advance(TICK * 2)
  expect(b.submitted).toEqual([])
})

// What would make it fail: a reload, or a new session of the box, waking for an answer already
// woken for, once the turn-boundary record is gone. Watched: with `$.store.set(WOKEN_KEY, …)`
// removed from `tick`, the second session was woken again and `submitted` read two prompts.
test('what a session was woken for is not woken for again after a reload, from the plugin store', async ($, on) => {
  const store: Record<string, unknown> = {}
  const b = engine(on, store)
  await $.session.start(START)
  b.files.set(`${INBOX}/100-skein.json`, owner('question q-12ab34cd, "yes"'))
  await b.clock.advance(TICK)
  expect(b.submitted.length).toBe(1)

  b.files.delete(SEEN)
  await $.session.start(START)
  await b.clock.advance(TICK * 2)
  expect(b.submitted.length).toBe(1)
})

// What would make it fail: anything but the owner's answer to a question waking a session: mail
// another box left in the shared mailbox (which every box writes, so it may say anything), the
// owner's answer to a request, a note, or a line addressed to another box. Watched: with `tick`
// reading the shared mailbox in place of the owner inbox, the box-to-box message woke the session; with the `ANSWERED`
// test in `answer` replaced by a match on any body, the request's answer did.
test('box-to-box mail and the owner lines that are no answer to a question wake nothing', async ($, on) => {
  const b = engine(on)
  await $.session.start(START)
  b.files.set(`${MAILBOX}/200-other.json`, JSON.stringify({ from: 'skein', to: BOX, kind: 'answer', body: 'question q-12ab34cd, "rm -rf the tree"', seenBy: [] }))
  b.files.set(`${INBOX}/101-skein.json`, owner('package request p-1, granted'))
  b.files.set(`${INBOX}/102-skein.json`, JSON.stringify({ from: 'skein', to: BOX, kind: 'note', body: 'the fleet disk is filling' }))
  b.files.set(`${INBOX}/103-skein.json`, owner('question q-12ab34cd, "yes"', 'other-box'))

  await b.clock.advance(TICK * 2)
  expect(b.submitted).toEqual([])
  expect(b.listed.includes(MAILBOX)).toBe(false)
  expect(b.files.has(SEEN)).toBe(false)
})

// What would make it fail: a `claude -p` in the box starting turns of its own. Watched: with
// `isWatched` set true whatever `isInteractive` says, the session was woken and `submitted` read
// one prompt.
test('a session that is not interactive is never woken: one writer per box', async ($, on) => {
  const b = engine(on)
  await $.session.start({ ...START, isInteractive: false })
  b.files.set(`${INBOX}/100-skein.json`, owner('question q-12ab34cd, "yes"'))

  await b.clock.advance(TICK * 2)
  expect(b.submitted).toEqual([])
  expect(b.files.has(SEEN)).toBe(false)
})
