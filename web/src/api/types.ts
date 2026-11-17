/** Rust 侧 serde 结构体的 TypeScript 对应类型。字段名保持 snake_case。 */

export interface DiskInfo {
  total: number;
  used: number;
  free: number;
}

export interface CategorySize {
  key: string;
  name: string;
  tier: string;
  describe: string;
  warning: string;
  note: string;
  size: number;
  count: number;
  requires_confirm: boolean;
  implemented: boolean;
}

export interface SystemItem {
  key: string;
  name: string;
  describe: string;
  size: number;
  count: number;
  safe_to_clean: boolean;
  is_command: boolean;
}

export interface HiberInfo {
  enabled: boolean;
  size: number;
  path: string;
  description: string;
}

export interface BigFile {
  path: string;
  size: number;
  mtime: number;
}

export interface DupGroup {
  hash: string;
  size: number;
  files: string[];
  mtimes: number[];
}

export interface ImRecord {
  path: string;
  size: number;
  mtime: number;
  kind: string;
  sub: string;
  month: string;
  name: string;
}

export interface DeleteReport {
  freed: number;
  deleted: number;
  failed: string[];
  reasons: string[];
}

export interface ScanStatus {
  active: boolean;
  stage: string;
  cancelled: boolean;
  done: number;
  total: number;
  found: number;
}

export interface TaskStatus {
  id: string;
  title: string;
  stage: string;
  done: number;
  total: number;
  unit: string;
  active: boolean;
  cancellable: boolean;
  cancelling: boolean;
  outcome: string;
  summary: string;
  started_ms: number;
  finished_ms: number;
  mutating: boolean;
}

export interface FragReport {
  drive: string;
  percent: number | null;
  space_gb: number | null;
  count: number | null;
  free_gb: number | null;
  ssd: boolean | null;
}

export interface PrivacyItem {
  id: string;
  section: string;
  name: string;
  count: number;
  count_available: boolean;
  files: string[];
  action: string;
  note: string;
}

export interface PrivacyReport {
  files: number;
  sql: number;
  registry: number;
  failed: string[];
}

export interface DriveTarget {
  letter: string;
  free_bytes: number;
  total_bytes: number;
}

export interface MoveAnalysis {
  source: string;
  exists: boolean;
  size: number;
  file_count: number;
  is_junction: boolean;
  target_drives: DriveTarget[];
}

export interface ImPreview {
  text: string;
  binary: boolean;
  truncated: boolean;
  size: number;
}

export interface HistoryEntry {
  name: string;
  size: number;
  mtime: number;
}
