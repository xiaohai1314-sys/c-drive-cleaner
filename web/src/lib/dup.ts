import type { DupGroup } from "../api/types";

/** 每组默认保留：修改时间最新，其次路径最短。 */
export function pickKeeper(group: DupGroup): string {
  let best = group.files[0] ?? "";
  let bestKey: [number, number] | null = null;
  group.files.forEach((path, index) => {
    const key: [number, number] = [group.mtimes[index] || 0, -path.length];
    if (
      !bestKey ||
      key[0] > bestKey[0] ||
      (key[0] === bestKey[0] && key[1] > bestKey[1])
    ) {
      bestKey = key;
      best = path;
    }
  });
  return best;
}

/** 按规则重算勾选：返回所有非保留副本。 */
export function applyKeeperRule(
  groups: DupGroup[],
  rule: "newest" | "shortest",
): Set<string> {
  const checked = new Set<string>();
  for (const group of groups) {
    let keeper = group.files[0];
    if (rule === "newest") {
      let maxTime = -1;
      group.files.forEach((path, index) => {
        const time = group.mtimes[index] || 0;
        if (time > maxTime) {
          maxTime = time;
          keeper = path;
        }
      });
    } else {
      let minLength = Number.MAX_SAFE_INTEGER;
      group.files.forEach((path) => {
        if (path.length < minLength) {
          minLength = path.length;
          keeper = path;
        }
      });
    }
    for (const path of group.files) {
      if (path !== keeper) checked.add(path);
    }
  }
  return checked;
}
