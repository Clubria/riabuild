#!/usr/bin/env node
// ============================================================================
//  Claude Code context-window bar  —  shows how full your context window is.
//  e.g.  █████░░░░░ 54%   (green → yellow → orange → blinking red 💀)
// ----------------------------------------------------------------------------
//  Shipped with riabuild and written to ~/.riabuild/claude-statusline.js by the
//  `claude_statusline` setup task. Edits here are overwritten on the next run.
//
//  The team's Claude Code settings point at this file:
//
//       "statusLine": {
//         "type": "command",
//         "command": "node ~/.riabuild/claude-statusline.js"
//       }
//
//  `node` resolves to the Node riabuild installed, which is on PATH alongside
//  the `c` launcher inside the Clubria environment shell.
//
//  This script is compiled into the riabuild binary rather than served by
//  riabuild-web. A status line is code Claude Code runs on every render, and
//  riabuild distributes code through signed Homebrew releases only.
// ============================================================================

let input = '';
process.stdin.on('data', (c) => (input += c));
process.stdin.on('end', () => {
  try {
    const data = JSON.parse(input || '{}');
    const remaining = data.context_window?.remaining_percentage; // 0-100 left
    if (remaining == null) return; // nothing to show

    // Claude Code reserves a buffer for auto-compaction (~16.5% by default).
    // Measure the USABLE window so the bar reads 100% when compaction kicks in.
    const totalCtx = data.context_window?.total_tokens || 1_000_000;
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
    let out;
    if (used < 50)      out = `\x1b[32m${bar} ${used}%\x1b[0m`;
    else if (used < 65) out = `\x1b[33m${bar} ${used}%\x1b[0m`;
    else if (used < 80) out = `\x1b[38;5;208m${bar} ${used}%\x1b[0m`;
    else                out = `\x1b[5;31m\u{1F480} ${bar} ${used}%\x1b[0m`; // 💀 blinking

    process.stdout.write(out);
  } catch {
    // Silent fail: never break the prompt line.
  }
});
