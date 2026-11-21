// Capture the same native views before/after a UI change. Only synthetic local data.
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const [directory, executable = 'target/debug/board-ui'] = process.argv.slice(2);
if (!directory) throw Error('usage: node ui/inspect-readability.mjs NEW_DIRECTORY [BOARD_UI]');
const out = resolve(directory);
mkdirSync(out);
const scratch = mkdtempSync(join(tmpdir(), 'board-ui-contrast-'));
const env = { ...process.env, BOARD_DB: join(scratch, 'board.sqlite3'), BOARD_USER: 'operator', RUNE_DIRS_ROOT: join(scratch, 'rune') };
delete env.BOARD_PROFILE;
delete env.BOARD_TOKEN;
if (process.platform === 'linux' && !env.VK_DRIVER_FILES && !env.VK_ICD_FILENAMES && !existsSync('/dev/dri') && existsSync('/usr/lib/chromium/vk_swiftshader_icd.json')) {
  env.VK_DRIVER_FILES = '/usr/lib/chromium/vk_swiftshader_icd.json';
}
try {
  const tap = key => [{ t: 0.15, type: 'tap', key }];
  for (const [name, steps] of [['activity', []], ['tasks', tap('tasks')], ['access', tap('workspace')]]) {
    const script = join(scratch, 'input.json');
    writeFileSync(script, JSON.stringify(steps));
    for (const theme of ['dark', 'light']) {
      const base = join(out, `${name}-${theme}`);
      const result = spawnSync(resolve(executable), ['--script',script,'--size','1100x680','--at','0.8','--fps','12','--scale','1','--snap',`${base}.png`,'--tree',`${base}.json`], { env: {...env, BOARD_UI_THEME: theme}, encoding:'utf8',timeout:60000 });
      if (result.status !== 0) throw result.error ?? Error(result.stderr);
      const tree=JSON.parse(readFileSync(`${base}.json`, 'utf8'));
      if (tree.warnings.length) throw Error(tree.warnings.join('; '));
      console.log(`${name}-${theme}`);
    }
  }
} finally { rmSync(scratch, { recursive: true, force: true }); }
