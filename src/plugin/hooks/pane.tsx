// The skein pane (SKEIN-1440, step 3): stacked panels beside the chat, each gone while it has
// nothing to say.
//
// * BOX: disk, memory and CPU against the box's limits, with the CPU's last minutes as a sparkline,
//   its processes, what skein asks of it, the disk skein asks it to free and unread mail, dim: the
//   agent clears the disk and reads the mail, and `/skein clean` is the person's override
//   (SKEIN-1460);
// * WAITING ON YOU, amber, with a count: each thing worded as what to do;
// * the lanes: every lane skein made in the box, and each worktree a subagent worked in, along the
//   way from made to merged, as far as the session sees (`board.ts`). A number key opens one;
// * the queue: the fleet's cores, one cell each, and the box's own jobs, another box's named with
//   it. A box has no address of its own to link to, so none is linked;
// * the spend, today and over 7 days, as skein-server wrote it;
// * the tracker items the session holds, with the lease time left;
// * a short log of what changed, as the looks saw it.
//
// `/skein clean` draws the disk list instead, a Clean button on each item that can go now. Docked
// it draws in full; inline above the prompt, or in a body under 44 cells, it is the narrow
// summary, a line each and eight at most. When the pane opens and on what view is `band.tsx`'s,
// which reads everything drawn here; nothing here reads a file or runs a process but the lease
// file's time (`tracker.ts`).

import type { EngineInterface as Engine, On } from 'claude-code'

import type { SkeinBand, SkeinBandJobs, SkeinCleanRow, SkeinPane, SkeinSharedCache } from '../types'

import { fmtCost, laneRuns, modelName } from './agents-panel'
import { PANE, askLine, cleanFromPane, diskLine, human, paneData, quietAskLine, showInPane, waitingThings } from './band'
import type { Waiting } from './band'
import { boardLanes, GLYPH, pipeline, STAGES, stageWords } from './board'
import type { BoardLane, Stage } from './board'
import { leasesLeft, name as leaseName } from './tracker'
import { bar, cells, cut, sparkline, vitalsLine, workLines } from './vitals'

// The disk list's longest rows in the pane; `skein_clean` lists every one.
const TOP_ROWS = 8

// Docked panels are drawn this wide at most, so a wide dock does not spread them thin.
const MOST_COLUMNS = 72

// Under this many cells the pane draws the narrow summary even when docked.
const NARROW_UNDER = 44

// The narrow summary's lanes, and the log's lines docked.
const NARROW_LANES = 3
const LOG_LINES = 4

/** How a span is coloured: skein's own, amber, dim, done or failed. */
type Tone = 'skein' | 'warning' | 'dim' | 'success' | 'error'
type Span = { text: string; tone?: Tone; bold?: boolean }

/** One row of the pane: spans, and where a row opens a lane, the Button between them. */
type Line = { key: string; spans: Span[]; press?: { hotkey: string; label: string; lane: string }; after?: Span[] }

export function register(on: On): void {
  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Button, Text } = $.ui.resolve(e)
    const [{ found, jobs, history, kept, view, said }, runs, held, now, box, state] = await Promise.all([
      paneData($),
      laneRuns($),
      leasesLeft($).catch(() => []),
      $.clock.now(),
      $.env.get('SKEIN_BOX'),
      $.env.get('SKEIN_STATE'),
    ])
    if (!state) return <Text dimColor>This session is not in a skein box.</Text>

    const isNarrow = e.props.placement === 'inline' || e.props.bodyColumns < NARROW_UNDER
    const width = Math.max(20, Math.min(e.props.bodyColumns, MOST_COLUMNS))
    const lanes = boardLanes(kept.lanes, runs, jobs)
    const opened = view.view === 'lane' ? lanes.find(l => l.path === view.lane) : undefined
    const waiting = found === null ? [] : waitingThings(found)
    const facts: Facts = { found, jobs, history, kept, lanes, waiting, held, now, box: box || null }

    const tone = (s: Span) => ({
      color: s.tone === 'skein' ? 'suggestion' : s.tone === 'warning' ? 'warning' : s.tone === 'success' ? 'success' : s.tone === 'error' ? 'error' : undefined,
      dimColor: s.tone === 'dim' ? true : undefined,
      bold: s.bold ? true : undefined,
    })
    const spans = (list: readonly Span[]) => list.map(s => <Text {...tone(s)}>{s.text}</Text>)
    const drawn = (lines: readonly Line[]) =>
      lines.map(l => (
        <Box key={`row:${l.key}`}>
          {spans(l.spans)}
          {l.press !== undefined && (
            <Button
              key={`lane:${l.press.hotkey}`}
              label={l.press.label}
              hotkey={l.press.hotkey}
              plain
              onPress={() => showInPane($, { view: 'lane', lane: l.press?.lane ?? null })}
            />
          )}
          {l.after !== undefined && spans(l.after)}
        </Box>
      ))
    const back = <Button key="pane:back" label="Back" hotkey="b" plain onPress={() => showInPane($, { view: 'board', lane: null })} />

    if (opened !== undefined) {
      return (
        <Box flexDirection="column">
          {drawn([header(facts, width), ...laneOpened(opened, facts, width)])}
          {back}
        </Box>
      )
    }

    // The disk list, with a Clean button on each item that can go now.
    const clean = found?.clean ?? null
    const own = (clean?.rows ?? []).filter(r => r.where === 'box')
    const shown = [...own].sort((a, b) => b.bytes - a.bytes).slice(0, TOP_ROWS)
    const shared = clean?.shared ?? null
    const sharedRows = (clean?.rows ?? []).filter(r => r.where === 'shared' && r.safe_now)
    const remove = (paths: string[]) => () => void cleanFromPane($, paths)
    const itemRow = (row: SkeinCleanRow) => {
      const what = `${row.in_box}, ${row.kind}, ${human(row.bytes)}`
      if (!row.safe_now) return <Text dimColor>{what}: {row.why_not ?? 'not removable now'}</Text>

      return (
        <Box>
          <Button key={`clean:${row.path}`} label="Clean" onPress={remove([row.path])} />
          <Text> {what}</Text>
        </Box>
      )
    }
    const sharedRow = (cache: SkeinSharedCache) => {
      const what = `The repo's shared cache, ${cache.path}, ${human(cache.bytes)}. Not counted against this box.`
      if (!cache.safe_to_clean_now || sharedRows.length === 0) {
        return <Text dimColor>{what} {cache.why_not ?? 'Nothing in it can be removed now.'}</Text>
      }

      return (
        <Box>
          <Button key="clean:shared" label="Clean" onPress={remove(sharedRows.map(r => r.path))} />
          <Text> {what}</Text>
        </Box>
      )
    }
    const isCleanView = view.view === 'clean'
    const cleanList = (
      <Box flexDirection="column">
        <Text bold color="suggestion">
          disk · what this box can remove
        </Text>
        <Text>{diskLine(found)}</Text>
        {isCleanView && found?.disk?.ask && <Text bold color="warning">{askLine(found.disk.ask)}</Text>}
        {clean === null && <Text dimColor>skein has not listed what this box can remove yet.</Text>}
        {clean !== null && own.length === 0 && <Text dimColor>Nothing in this box is on skein's list.</Text>}
        {shown.map(itemRow)}
        {own.length > shown.length && <Text dimColor>And {own.length - shown.length} more. skein_clean lists every one.</Text>}
        {shared !== null && sharedRow(shared)}
        {said !== null && <Text>{said}</Text>}
      </Box>
    )

    if (isCleanView) {
      return (
        <Box flexDirection="column">
          {drawn([header(facts, width)])}
          {cleanList}
          {back}
        </Box>
      )
    }
    if (isNarrow) return <Box flexDirection="column">{drawn(narrow(facts, width))}</Box>

    const top = [header(facts, width), ...boxPanel(facts, width), ...waitingPanel(facts, width)]
    const rest = [...lanesPanel(facts, width), ...queuePanel(facts, width), ...spendPanel(facts, width), ...holdingPanel(facts, width), ...logPanel(facts, width)]
    const isEmpty = top.length === 1 && rest.length === 0

    return (
      <Box flexDirection="column">
        {drawn(top)}
        {drawn(rest)}
        {isEmpty && <Text dimColor>Nothing from skein to show yet.</Text>}
      </Box>
    )
  })
}

// ------------------------------------------------------------------------------------------------
// Panels: each a list of lines, empty when the panel has nothing to say
// ------------------------------------------------------------------------------------------------

/** What the panels draw from. */
type Facts = {
  found: SkeinBand | null
  jobs: SkeinBandJobs | null
  history: readonly { minute: number; cores: number }[]
  kept: SkeinPane
  lanes: BoardLane[]
  waiting: Waiting[]
  held: { lease: Parameters<typeof leaseName>[0]; endsAt: number | null }[]
  now: number
  box: string | null
}

function header(f: Facts, width: number): Line {
  const n = f.waiting.length
  const where = f.box ?? 'this box'
  const state = n === 0 ? 'ok' : `${n} waiting on you`

  return {
    key: 'head',
    spans: [
      { text: 'SKEIN', tone: 'skein', bold: true },
      { text: cut(` · ${where} · `, Math.max(0, width - 5 - state.length)), tone: 'dim' },
      { text: state, tone: n === 0 ? 'success' : 'warning' },
    ],
  }
}

function boxPanel(f: Facts, width: number): Line[] {
  const found = f.found
  if (found === null) return []
  const inner = width - 4
  const rows: Span[][] = []
  const disk = found.disk
  if (disk !== null && disk.mb !== null) rows.push([{ text: gauge('disk', disk.mb * MIB, disk.limit_mb === null ? null : disk.limit_mb * MIB) }])
  if (found.memory !== null) rows.push([{ text: gauge('mem ', found.memory.bytes, found.memory.limit) }])
  if (found.cpu !== null && f.history.length > 0) {
    const n = Math.max(4, Math.min(14, inner - 20))
    const spark = sparkline(f.history, n)
    rows.push([{ text: `cpu  ${spark}  ${found.cpu.toFixed(1)} cores · ${cells(spark)}m` }])
  }
  const ask = found.disk?.ask ?? null
  const kinds = [...(ask ? ['disk'] : []), ...found.asked].map(k => (k === 'pids' ? 'processes' : k))
  const asked = kinds.length === 0 ? 'nothing asked' : `skein asks about ${kinds.join(', ')}`
  // Amber for what the agent cannot clear by itself, and for a disk ask skein holds for; a disk ask
  // it does not hold for, alone, is dim (SKEIN-1475).
  const askTone: Tone | undefined = found.asked.length > 0 || ask?.held ? 'warning' : kinds.length > 0 ? 'dim' : undefined
  const pids = found.pids
  if (pids !== null && pids.current !== null) {
    rows.push([{ text: `processes ${pids.current}${pids.max === null ? '' : ` / ${pids.max}`} · ` }, { text: asked, tone: askTone }])
  } else if (rows.length > 0) {
    rows.push([{ text: asked, tone: askTone }])
  }
  // A held ask is in WAITING ON YOU instead.
  if (ask && !ask.held) for (const part of wrap(quietAskLine(ask), inner)) rows.push([{ text: part, tone: 'dim' }])
  const mail = found.mail ?? 0
  // Mail is the agent's to read: a dim line, never amber.
  if (mail > 0) rows.push([{ text: `✉ ${mail} unread, for the agent's next turn`, tone: 'dim' }])
  if (rows.length === 0) return []

  return framed('box', 'BOX', null, rows, width, 'skein')
}

function waitingPanel(f: Facts, width: number): Line[] {
  if (f.waiting.length === 0) return []
  const inner = width - 4
  const rows: Span[][] = []
  for (const w of f.waiting) {
    wrap(w.action, inner - 2).forEach((part, i) => rows.push([{ text: `${i === 0 ? '⚑ ' : '  '}${part}`, tone: 'warning' }]))
  }

  return framed('waiting', 'WAITING ON YOU', String(f.waiting.length), rows, width, 'warning')
}

function lanesPanel(f: Facts, width: number): Line[] {
  if (f.lanes.length === 0) return []
  const atWork = f.lanes.filter(l => l.run?.status === 'running').length
  const keys = Math.min(9, f.lanes.length)
  const head = `lanes · ${f.lanes.length} · ${atWork} at work`
  const hint = keys === 1 ? '1 opens it' : `1-${keys} open one`
  const out: Line[] = [
    {
      key: 'lanes',
      spans: [{ text: head, tone: 'skein', bold: true }, { text: `${' '.repeat(Math.max(1, width - head.length - hint.length))}${hint}`, tone: 'dim' }],
    },
  ]
  const nameW = Math.min(10, Math.max(...f.lanes.map(l => cells(l.name))))
  f.lanes.forEach((l, i) => {
    const words = `${l.name.padEnd(nameW)} ${(l.item ?? '').padEnd(10)} ${pipeline(l.stage, 8)} ${stageWords(l)}`
    out.push(laneLine(l, i, words, width))
  })
  // The legend, broken before a `›` where the pane is too narrow for it whole, so no stage is cut.
  const legend: string[] = []
  for (const [i, stage] of STAGES.entries()) {
    const last = legend.at(-1)
    const next = i === 0 ? `    ${stage}` : `${last} › ${stage}`
    if (last === undefined || cells(next) <= width) legend[Math.max(0, legend.length - 1)] = next
    else legend.push(`    › ${stage}`)
  }
  legend.forEach((text, i) => out.push({ key: `stages:${i}`, spans: [{ text: cut(text, width), tone: 'dim' }] }))

  return out
}

function queuePanel(f: Facts, width: number): Line[] {
  const view = f.jobs
  if (view === null || view.jobs.length === 0) return []
  const running = view.jobs.filter(j => j.state === 'granted').length
  const waiting = view.jobs.length - running
  const yours = [running > 0 ? `${running} running` : null, waiting > 0 ? `${waiting} waiting` : null].filter(p => p !== null).join(', ')
  const fleet = view.fleet
  const named = f.lanes.map(l => ({ name: l.name, worktree: l.path, item: l.item, description: '', lock: null }))
  // The box's own jobs, then what goes before its first waiting one, each named with its box on a
  // line of its own so a narrow pane never cuts the name. A box has no address to link to.
  const lines = workLines({ ...view, ahead: [] }, named, width).filter(w => w.key.startsWith('job:'))
  const ahead = view.ahead.map((a, i) => ({
    key: `ahead:${i}`,
    spans: [{ text: cut(`  ahead, from ${a.box}: ${a.command}${a.cores > 0 ? ` · ${a.cores} cores` : ''}`, width), tone: 'dim' as const }],
  }))

  return [
    { key: 'queue', spans: [{ text: cut(`queue · yours: ${yours}`, width), tone: 'skein', bold: true }] },
    ...(fleet === null
      ? []
      : [{ key: 'fleet', spans: [{ text: cut(`fleet ${fleetCells(fleet.cores, fleet.granted)} ${fleet.granted}/${fleet.cores} cores · ${fleet.running} running`, width), tone: 'dim' as const }] }]),
    ...lines.map(w => ({ key: w.key, spans: [{ text: w.text, tone: w.isWaiting ? ('dim' as const) : undefined }] })),
    ...ahead,
  ]
}

function spendPanel(f: Facts, width: number): Line[] {
  const spend = f.found?.spend ?? null
  if (spend === null) return []
  const plus = spend.unpriced ? '+' : ''

  return [
    { key: 'spend', spans: [{ text: `spend · ≈${dollars(spend.today)}${plus} today · ≈${dollars(spend.week)}${plus} 7d`, tone: 'skein', bold: true }] },
    ...wrap(`estimated at API prices as of ${spend.at} UTC${spend.unpriced ? '; + leaves out a model skein has no price for' : ''}`, width).map(
      (text, i) => ({ key: `spend-note:${i}`, spans: [{ text, tone: 'dim' as const }] }),
    ),
  ]
}

function holdingPanel(f: Facts, width: number): Line[] {
  if (f.held.length === 0) return []
  const n = f.held.length
  const out: Line[] = [{ key: 'holding', spans: [{ text: `holding · ${n} ${n === 1 ? 'item' : 'items'}`, tone: 'skein', bold: true }] }]
  for (const { lease, endsAt } of f.held) {
    const left = endsAt === null ? null : endsAt - f.now
    const ttl = lease.renewsForMs
    const gauge = left === null || ttl === null ? '' : `${leaseBar(left, ttl, 11)} `
    const words = left === null ? 'lease time not said' : left <= 0 ? 'runs out now' : `${Math.max(1, Math.round(left / 60_000))}m left`
    const head = ` ${leaseName(lease)} lease ${gauge}${words}`
    // The title after it where there is room for some of it, else on the line below.
    const room = width - cells(head) - 3
    const title = lease.title !== null && lease.readableId !== null ? lease.title : null
    out.push({
      key: `held:${lease.workItemId}`,
      spans: [{ text: head }, ...(title !== null && room >= 12 ? [{ text: ` · ${cut(title, room)}`, tone: 'dim' as const }] : [])],
    })
    if (title !== null && room < 12) out.push({ key: `held-title:${lease.workItemId}`, spans: [{ text: cut(`   ${title}`, width), tone: 'dim' }] })
  }

  return out
}

function logPanel(f: Facts, width: number): Line[] {
  const shown = f.kept.log.slice(-LOG_LINES)

  return shown.map((l, i) => ({ key: `log:${i}`, spans: [{ text: cut(`${l.at} ${l.who.padEnd(8)} ${l.what}`, width), tone: 'dim' }] }))
}

/** The narrow summary: a line each, eight at most. */
function narrow(f: Facts, width: number): Line[] {
  const found = f.found
  const out: Line[] = []
  const vitals = vitalsLine(
    {
      disk: found?.disk && found.disk.mb !== null ? { mb: found.disk.mb, limit_mb: found.disk.limit_mb } : null,
      memory: found?.memory ?? null,
      cpu: found?.cpu === null || found?.cpu === undefined ? [] : f.history,
      spend: found?.spend ?? null,
    },
    width,
  )
  out.push({ key: 'vitals', spans: [{ text: vitals, tone: 'skein' }] })
  if (f.waiting.length > 0) {
    const text = `⚑ ${f.waiting.length} waiting on you · ${f.waiting.map(w => w.short).join(' · ')}`
    out.push({ key: 'waiting', spans: [{ text: cut(text, width), tone: 'warning', bold: true }] })
  }
  const lanes = f.lanes.slice(0, NARROW_LANES)
  const nameW = Math.min(10, Math.max(0, ...lanes.map(l => cells(l.name))))
  lanes.forEach((l, i) => out.push(laneLine(l, i, `${l.name.padEnd(nameW)} ${stageWords(l)}`, width)))
  const view = f.jobs
  if (view !== null && view.jobs.length > 0) {
    const running = view.jobs.filter(j => j.state === 'granted').length
    const first = view.jobs.filter(j => j.state === 'waiting' && j.position !== null).map(j => j.position ?? 0)
    const parts = [running > 0 ? `${running} running` : null, first.length > 0 ? `#${Math.min(...first)}` : null].filter(p => p !== null)
    const slots = view.fleet === null ? '' : ` ${fleetCells(view.fleet.cores, view.fleet.granted)}`
    out.push({ key: 'queue', spans: [{ text: cut(`queue${slots} · yours: ${parts.join(', ')}`, width) }] })
  }
  if (f.held.length > 0) {
    const ids = f.held.map(({ lease, endsAt }) => {
      const left = endsAt === null ? null : Math.max(0, Math.round((endsAt - f.now) / 60_000))
      return `${leaseName(lease)}${left === null ? '' : ` ${left}m`}`
    })
    out.push({ key: 'holding', spans: [{ text: cut(`holding ${ids.join(', ')}`, width) }] })
  }
  if (f.lanes.length > 0) {
    const more = f.lanes.length - lanes.length
    const keys = lanes.length === 1 ? '1 to open the lane' : `1-${lanes.length} to open a lane`
    out.push({ key: 'hint', spans: [{ text: cut(`ctrl+x tab, then ${keys}${more > 0 ? ` · ${more} more docked` : ''}`, width), tone: 'dim' }] })
  }

  return out
}

/** One lane opened: what a coordinator reads before it merges, as far as the session records it. */
function laneOpened(l: BoardLane, f: Facts, width: number): Line[] {
  const run = l.run
  const out: Line[] = [
    {
      key: 'lane-head',
      spans: [
        { text: `${GLYPH[l.stage]} `, tone: stageTone(l.stage) },
        { text: cut(`${l.name} · ${l.item ?? 'no item'} · ${l.branch ?? 'no branch'}`, width - 2), bold: true },
      ],
    },
  ]
  if (l.title !== null) for (const [i, part] of wrap(l.title, width - 2).entries()) out.push({ key: `lane-title:${i}`, spans: [{ text: `  ${part}`, tone: 'dim' }] })
  else if (l.item !== null) out.push({ key: 'lane-title', spans: [{ text: `  ${l.item}, not claimed yet`, tone: 'dim' }] })
  if (l.title !== null && !l.isHeld) out.push({ key: 'lane-released', spans: [{ text: '  no longer held', tone: 'dim' }] })
  const reason = l.job !== null && l.job.state === 'waiting' && l.job.reason !== '' ? ` · ${l.job.reason}` : ''
  out.push({ key: 'lane-stage', spans: [{ text: cut(`  ${pipeline(l.stage, 16)} ${stageWords(l)}${reason}`, width) }] })
  const agent =
    run === null
      ? 'none of this session yet'
      : [run.status === 'running' ? 'running' : run.status, modelName(run.model), duration((run.endedAt ?? f.now) - run.startedAt), fmtCost(run.costUsd, run.isUnpriced)].join(' · ')
  const row = (key: string, name: string, value: string): Line => ({ key, spans: [{ text: cut(`  ${name.padEnd(9)}${value}`, width) }] })
  out.push(row('lane-agent', 'agent', agent))
  if (l.job !== null) out.push(row('lane-job', 'job', l.job.command))
  out.push(row('lane-tree', 'worktree', l.path))
  out.push(row('lane-record', 'record', l.isMadeBySkein ? (l.phase ?? 'no phase') : 'none: made by hand'))
  for (const [i, part] of wrap('Its commits, tests and CI are not recorded for a lane yet.', width - 2).entries()) {
    out.push({ key: `lane-gap:${i}`, spans: [{ text: `  ${part}`, tone: 'dim' }] })
  }

  return out
}

// ------------------------------------------------------------------------------------------------
// Drawing
// ------------------------------------------------------------------------------------------------

/** A lane's row: its mark, then a Button its number presses, which opens it. */
function laneLine(l: BoardLane, i: number, words: string, width: number): Line {
  const hotkey = i < 9 ? String(i + 1) : null
  const mark = { text: `${GLYPH[l.stage]} `, tone: stageTone(l.stage) }
  // `1: ` is the painter's own, before the label.
  if (hotkey === null) return { key: `lane:${l.path}`, spans: [mark, { text: cut(`   ${words}`, width - 2) }] }

  return { key: `lane:${l.path}`, spans: [mark], press: { hotkey, label: cut(words, width - 5), lane: l.path } }
}

function stageTone(s: Stage): Tone | undefined {
  return s === 'done' ? 'success' : s === 'failed' ? 'error' : s === 'queued' || s === 'made' || s === 'stopped' || s === 'merged' ? 'dim' : undefined
}

/** A panel in a frame of `tone`: its title in the top edge, a count at its end, each row padded. */
function framed(key: string, title: string, count: string | null, rows: readonly Span[][], width: number, tone: Tone): Line[] {
  const inner = width - 4
  const end = count === null ? '' : ` ${count} `
  const top = `╭ ${title} ${'─'.repeat(Math.max(1, width - 4 - title.length - end.length))}${end}╮`
  const out: Line[] = [{ key: `${key}:top`, spans: [{ text: top, tone, bold: tone === 'warning' ? true : undefined }] }]
  rows.forEach((row, i) => {
    const kept: Span[] = []
    let room = inner
    for (const s of row) {
      const text = cut(s.text, room)
      room -= cells(text)
      kept.push({ ...s, text })
      if (room <= 0) break
    }
    out.push({ key: `${key}:${i}`, spans: [{ text: '│ ', tone }, ...kept, { text: `${' '.repeat(Math.max(0, room))} │`, tone }] })
  })
  out.push({ key: `${key}:end`, spans: [{ text: `╰${'─'.repeat(width - 2)}╯`, tone }] })

  return out
}

/** `disk ▮▮▮▯▯▯▯▯▯▯  2.7 / 10 G   27%`, or the figure alone where the box has no limit. */
function gauge(name: string, used: number, limit: number | null): string {
  if (limit === null) return `${name} ${amount(used, used)}`
  const pct = `${Math.round((used / Math.max(1, limit)) * 100)}%`

  return `${name} ${bar(used, limit, 10)}  ${amount(used, limit)} / ${amount(limit, limit)} ${limit >= G ? 'G' : 'M'}  ${pct.padStart(4)}`
}

/** A figure in the unit its limit is in: tenths of a gigabyte, or whole megabytes under one. */
function amount(n: number, limit: number): string {
  if (limit < G) return String(Math.round(n / MIB))
  const g = n / G

  return Math.abs(g - Math.round(g)) < 0.05 && n === limit ? String(Math.round(g)) : g.toFixed(1)
}

/** The fleet's cores, one cell each, `▣` granted and `□` free; past 24 a cell is several cores. */
function fleetCells(cores: number, granted: number): string {
  const per = Math.max(1, Math.ceil(cores / 24))
  const n = Math.ceil(cores / per)
  const full = Math.min(n, Math.round(granted / per))

  return '▣'.repeat(full) + '□'.repeat(n - full)
}

/** `━━━━━━━───`: the lease's time left of what one renewal gives it, in `n` cells. */
function leaseBar(left: number, ttl: number, n: number): string {
  const filled = Math.max(0, Math.min(n, Math.round((left / Math.max(1, ttl)) * n)))

  return '━'.repeat(filled) + '─'.repeat(n - filled)
}

/** Words cut into lines of at most `width` cells; a word longer than a line is cut. */
function wrap(text: string, width: number): string[] {
  const out: string[] = []
  let line = ''
  for (const word of text.replace(/\s+/g, ' ').trim().split(' ')) {
    if (line === '') line = cut(word, width)
    else if (cells(line) + 1 + cells(word) <= width) line = `${line} ${word}`
    else {
      out.push(line)
      line = cut(word, width)
    }
  }
  if (line !== '') out.push(line)

  return out
}

/** `2h 10m`, `12m`, `40s`. */
function duration(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000))
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)

  return m < 60 ? `${m}m` : `${Math.floor(m / 60)}h ${m % 60}m`
}

/** `$4.20`, or whole dollars from $100. */
function dollars(n: number): string {
  return n < 100 ? `$${n.toFixed(2)}` : `$${Math.round(n).toLocaleString('en-US')}`
}

const MIB = 1024 * 1024
const G = 1024 * MIB
