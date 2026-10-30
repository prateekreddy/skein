// Tracker discipline (SKEIN-1366): what this session holds in the tracker, and a gentle word when a
// turn ends with a claim that has gone quiet or is about to run out.
//
// What is held comes from the tracker tools' own answers, heard as they pass: a `claim` adds the
// item, a `complete` or `release` that succeeded takes it away, and a `held` replaces the list with
// the tracker's. Nothing here asks the tracker anything, and nothing here ever releases: the box is
// told, and what to do is the box's call.
//
// The sync plugin keeps a claim alive from outside the session: its monitor renews the lease every
// couple of minutes and, on each renewal the tracker answers, rewrites this session's credential
// file (`~/.claude/sync/<session>.watch`). So that file's modification time is the last renewal.
// Only its time is read. The file holds a credential, and it is never opened here.
//
// A claim made before this module loaded (a resumed session, a CLI restart onto a new rollout) was
// never heard. So once a session, as soon as the tracker's `held` is listed, the module asks it
// itself: through `$.tool.call`, which runs every hook but the caller's, so the `tool.call` hook
// below hears the answer exactly as it hears the model's own `held`. It is asked only where the
// permission check already allows it, so no dialog ever opens for it (SKEIN-1400).
//
// The compaction keeper (keeper.ts) reads the same list, through `heldNow`.

import { atom, read, update } from 'claude-code'
import type { EngineInterface, On, SessionStartInput, TurnCompleteInput } from 'claude-code'

import type { SkeinLease, SkeinLeases } from '../types'

export const leases = atom({ plugin: 'skein', key: 'leases' } as const, null, {
  shape: 'leases-1',
})

/** True once this session has asked the tracker's `held` for itself, or decided not to. */
export const seeded = atom({ plugin: 'skein', key: 'leasesSeeded' } as const, false)

/** A tracker tool, by the name the engine lists it under: `mcp__plugin_sync_sync__claim` and the like. */
const TRACKER_TOOL = /^mcp__(?:.+_)?sync__([a-z_]+)$/

/** Turns a lease may go untouched before the box is reminded of it. */
export const IDLE_TURNS = 3

/** How close to running out a lease is when the box is warned. */
export const EXPIRY_WARN_MS = 3 * 60 * 1000

type Engine = EngineInterface

export function register(on: On): void {
  on('tool.call', { tool: TRACKER_TOOL }, async ($, e, next) => {
    const done = await next(e)
    const which = TRACKER_TOOL.exec(String(e.tool))
    if (which === null || done.deny !== undefined || done.isError === true) return done
    await heard($, which[1] ?? '', e as unknown as Record<string, unknown>, done.text ?? '')

    return done
  })
}

/** At the session's start, from the one `session.start` hook register.tsx holds (SKEIN-1387). */
export async function onStart($: Engine, _e: SessionStartInput): Promise<void> {
  await seed($)
}

/** As a turn ends, from the one `turn.complete` hook register.tsx holds (SKEIN-1387). */
export async function onTurnComplete($: Engine, e: TurnCompleteInput): Promise<void> {
  if (e.agentId !== undefined) return
  // The tracker's tools may not have been listed yet when the session started.
  try {
    await seed($)
  } catch {
    // Not asked this time; the reminders still run on what was heard.
  }
  await remind($)
}

/**
 * Ask the tracker's `held` once a session, so a claim made before this module loaded is known by
 * its item. Nothing before the tracker is listed; never where the permission check would ask.
 */
async function seed($: Engine): Promise<void> {
  if (await read($, seeded)) return
  const held = (await $.tool.list()).find(t => t.mcp && TRACKER_TOOL.exec(t.name)?.[1] === 'held')
  if (held === undefined) return
  let first = false
  await update($, seeded, was => {
    first = !was
    return true
  })
  if (!first) return
  const { decision } = await $.tool.check({ tool: held.name, input: {} })
  if (decision !== 'allow') return
  // Heard by the `tool.call` hook in `register`, like the model's own `held`.
  await $.tool.call({ tool: held.name })
}

/** What the session holds now, and whether there is a tracker at all, for the compaction keeper. */
export async function heldNow($: Engine): Promise<{ items: SkeinLease[]; hasTracker: boolean; hasClaimed: boolean }> {
  const kept = await read($, leases)
  const watch = await lastRenewal($)

  return {
    items: kept?.items ?? [],
    hasTracker: kept?.hasTracker === true || watch !== null,
    hasClaimed: watch !== null || (kept?.items.length ?? 0) > 0,
  }
}

/** One tracker tool's successful answer, folded into the list. */
async function heard($: Engine, tool: string, input: Record<string, unknown>, text: string): Promise<void> {
  const answer = parse(text)
  await update($, leases, prev => {
    const now: SkeinLeases = prev ?? { items: [], turns: 0, hasTracker: true }
    let items = now.items.slice()
    const id = typeof input.workItemId === 'string' ? input.workItemId : null
    const agentId = typeof input.agentId === 'string' ? input.agentId : null

    if (tool === 'claim') {
      const lease = field(answer, 'lease')
      const briefing = field(answer, 'briefing')
      const claimed = str(lease, 'workItemId')
      if (claimed !== null) {
        items = items.filter(l => l.workItemId !== claimed)
        items.push({
          workItemId: claimed,
          readableId: str(briefing, 'readableId'),
          title: str(briefing, 'title'),
          agentId,
          ...timing(lease),
          touchedTurn: now.turns,
          remindedIdleAt: null,
          remindedExpiryAt: null,
        })
      }
    } else if ((tool === 'complete' || tool === 'release') && id !== null) {
      items = items.filter(l => l.workItemId !== id)
    } else if (tool === 'held') {
      const listed = field(answer, 'leases')
      if (Array.isArray(listed)) {
        items = listed
          .filter(l => str(l, 'workItemId') !== null && field(l, 'thisSession') !== false)
          .map(l => {
            const workItemId = str(l, 'workItemId') as string
            const known = items.find(k => k.workItemId === workItemId)
            return {
              workItemId,
              readableId: known?.readableId ?? null,
              title: known?.title ?? null,
              agentId: known?.agentId ?? null,
              ...timing(l),
              touchedTurn: known?.touchedTurn ?? now.turns,
              remindedIdleAt: known?.remindedIdleAt ?? null,
              remindedExpiryAt: null,
            }
          })
      }
    }

    // Any other tracker call that names a held item is work on it: a comment, a capture from it.
    const named = JSON.stringify(input)
    items = items.map(l =>
      tool !== 'held' && named.includes(l.workItemId) ? { ...l, touchedTurn: now.turns } : l,
    )

    return { ...now, items, hasTracker: true }
  })
}

/** At the end of a main-conversation turn: count it, and say what has gone quiet or is running out. */
async function remind($: Engine): Promise<void> {
  const nowMs = await $.clock.now()
  const renewed = await lastRenewal($)
  const said: string[] = []
  const shown: string[] = []
  const after = await update($, leases, prev => {
    // `update` runs this again when another write landed first, so it starts from nothing each time.
    said.length = 0
    shown.length = 0
    if (prev === null) return prev
    const turns = prev.turns + 1
    const items = prev.items.map(l => {
      let next = l
      const quiet = turns - l.touchedTurn
      if (quiet >= IDLE_TURNS) {
        shown.push(`${name(l)} untouched for ${quiet} turns`)
        if (l.remindedIdleAt !== l.touchedTurn) {
          said.push(idleWords(l, quiet))
          next = { ...next, remindedIdleAt: l.touchedTurn }
        }
      }
      const ends = expiry(l, renewed)
      if (ends !== null && ends - nowMs <= EXPIRY_WARN_MS) {
        shown.push(`${name(l)}'s claim ${runsOut(ends - nowMs)}`)
        if (l.remindedExpiryAt !== ends) {
          said.push(expiryWords(l, ends - nowMs, renewed ?? l.expiresAt))
          next = { ...next, remindedExpiryAt: ends }
        }
      }
      return next
    })

    return { ...prev, turns, items }
  })
  if (after === null) return

  $.ui.status(shown.length > 0 ? `skein: ${shown.join(' · ')}` : undefined)
  if (said.length > 0) {
    await $.session.append({
      message: { type: 'user', content: [{ type: 'text', text: said.join('\n') }] },
    })
  }
}

/** When the lease runs out, as best this session can tell: the tracker's word, moved on by each renewal since. */
function expiry(l: SkeinLease, renewed: number | null): number | null {
  if (l.expiresAt === null) return null
  if (renewed === null || l.renewsForMs === null) return l.expiresAt

  return Math.max(l.expiresAt, renewed + l.renewsForMs)
}

/** When the sync plugin last renewed this session's claims: its credential file's time, never its contents. */
async function lastRenewal($: Engine): Promise<number | null> {
  const dir = (await $.env.get('SYNC_STATE_DIR')) || `${(await $.env.get('HOME')) ?? ''}/.claude/sync`
  try {
    const session = await $.session.id()
    if (session === '') return null
    const stat = await $.fs.stat(`${dir}/${session}.watch`)

    return stat.kind === 'file' ? stat.mtimeMs : null
  } catch {
    return null
  }
}

export function name(l: SkeinLease): string {
  return l.readableId ?? l.title ?? l.workItemId.slice(0, 8)
}

function idleWords(l: SkeinLease, quiet: number): string {
  return (
    `skein: you still hold ${name(l)} in the tracker, and no tracker call has named it for ${quiet} turns. ` +
    'If it is done, complete it with the evidence; if you have set it aside, release it. ' +
    'skein does not release anything for you.'
  )
}

function expiryWords(l: SkeinLease, leftMs: number, since: number | null): string {
  const quiet = since === null ? '' : `, and nothing has renewed it since ${clock(since)}`

  return (
    `skein: your claim on ${name(l)} ${runsOut(leftMs)}${quiet}. ` +
    'Call held to see where you stand, and claim it again if you are still working on it. ' +
    'skein does not release anything for you.'
  )
}

function runsOut(leftMs: number): string {
  const minutes = Math.round(leftMs / 60000)

  return minutes >= 1 ? `runs out in about ${minutes} min` : 'runs out about now'
}

function clock(ms: number): string {
  const d = new Date(ms)

  return `${String(d.getUTCHours()).padStart(2, '0')}:${String(d.getUTCMinutes()).padStart(2, '0')} UTC`
}

function timing(lease: unknown): { expiresAt: number | null; renewsForMs: number | null } {
  const expiresAt = time(str(lease, 'expiresAt'))
  const from = time(str(lease, 'heartbeatAt')) ?? time(str(lease, 'claimedAt'))

  return {
    expiresAt,
    renewsForMs: expiresAt !== null && from !== null && expiresAt > from ? expiresAt - from : null,
  }
}

function time(s: string | null): number | null {
  if (s === null) return null
  const t = Date.parse(s)

  return Number.isNaN(t) ? null : t
}

function parse(text: string): unknown {
  try {
    return JSON.parse(text)
  } catch {
    return null
  }
}

function field(v: unknown, key: string): unknown {
  return typeof v === 'object' && v !== null && key in v ? (v as Record<string, unknown>)[key] : undefined
}

function str(v: unknown, key: string): string | null {
  const got = field(v, key)

  return typeof got === 'string' && got !== '' ? got : null
}
