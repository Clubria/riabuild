#!/usr/bin/env node
// Written by riabuild's `claude_statusline` task — edits here are overwritten.

// Marks the status line the way the environment shell marks `PS1`, in the same
// bold blue: the Clubria settings are the reason this Claude Code session looks
// the way it does, and the line should say so without being asked.
//
// Printed even when there is no context data to draw, for the same reason the
// prompt label is unconditional — a marker that comes and goes marks nothing.
const LABEL = '\x1b[1;34m(clubria)\x1b[0m';

// How full the context window is: `█████░░░░░ 54%`, coloured green → yellow →
// orange → blinking red 💀. Returns '' when Claude Code sends no window data.
function contextBar(payload) {
  const remaining = payload.context_window?.remaining_percentage; // 0-100 left
  if (remaining == null) return '';

  // Claude Code reserves a buffer for auto-compaction (~16.5% by default).
  // Measure the USABLE window so the bar reads 100% when compaction kicks in.
  const totalCtx = payload.context_window?.total_tokens || 1_000_000;
  const acw = parseInt(process.env.CLAUDE_CODE_AUTO_COMPACT_WINDOW || '0', 10);
  const bufferPct = acw > 0
    ? Math.min(100, Math.max(0, (1 - acw / totalCtx) * 100))
    : 16.5;

  const usableRemaining = Math.max(0, ((remaining - bufferPct) / (100 - bufferPct)) * 100);
  const used = Math.max(0, Math.min(100, Math.round(100 - usableRemaining)));

  // 10-segment bar: filled blocks + empty blocks.
  const filled = Math.floor(used / 10);
  const bar = '█'.repeat(filled) + '░'.repeat(10 - filled); // █ / ░

  // Color by how full it is (ANSI): green <50, yellow <65, orange <80, blinking red >=80.
  if (used < 50)      return ` \x1b[32m${bar} ${used}%\x1b[0m`;
  else if (used < 65) return ` \x1b[33m${bar} ${used}%\x1b[0m`;
  else if (used < 80) return ` \x1b[38;5;208m${bar} ${used}%\x1b[0m`;
  else                return ` \x1b[5;31m\u{1F480} ${bar} ${used}%\x1b[0m`; // 💀 blinking
}

let input = '';
process.stdin.on('data', (c) => (input += c));
process.stdin.on('end', () => {
  let bar = '';
  try {
    bar = contextBar(JSON.parse(input || '{}'));
  } catch {
    // Silent fail: a broken bar still leaves a labelled status line.
  }
  process.stdout.write(LABEL + bar);
});
