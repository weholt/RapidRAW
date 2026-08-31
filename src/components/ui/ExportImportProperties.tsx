import { Progress } from './AppProperties';

export enum FileFormats {
  Jpeg = 'jpeg',
  Png = 'png',
  Tiff = 'tiff',
  Webp = 'webp',
  Jxl = 'jxl',
  Avif = 'avif',
  Cube = 'cube',
}

export const FILE_FORMATS: Array<FileFormat> = [
  { id: FileFormats.Jpeg, name: 'JPEG', extensions: ['jpg', 'jpeg'] },
  { id: FileFormats.Png, name: 'PNG', extensions: ['png'] },
  { id: FileFormats.Tiff, name: 'TIFF', extensions: ['tiff'] },
  { id: FileFormats.Webp, name: 'WebP', extensions: ['webp'] },
  { id: FileFormats.Jxl, name: 'JPEG XL', extensions: ['jxl'] },
  { id: FileFormats.Avif, name: 'AVIF', extensions: ['avif'] },
  { id: FileFormats.Cube, name: 'CUBE LUT', extensions: ['cube'] },
];

export const FILENAME_VARIABLES: Array<string> = [
  '{original_filename}',
  '{sequence}',
  '{YYYY}',
  '{MM}',
  '{DD}',
  '{hh}',
  '{mm}',
];

export const IMPORT_PATTERN_VERSION = 1 as const;
export const WORKFLOW_PROTOCOL_VERSION = 1 as const;

export const WORKFLOW_DEFAULT_ORDER = 100;
export const WORKFLOW_MIN_ORDER = 0;
export const WORKFLOW_MAX_ORDER = 1000;
export const WORKFLOW_DEFAULT_TIMEOUT_SECONDS = 60;
export const WORKFLOW_MIN_TIMEOUT_SECONDS = 1;
export const WORKFLOW_MAX_TIMEOUT_SECONDS = 600;

export type ImportOperation = 'copy' | 'move';
export type CollisionPolicy = 'skip' | 'renameWithSuffix' | 'error';
export type MissingTokenPolicy = 'empty' | 'fallback' | 'error';
export type MetadataToken =
  | 'originalFilename'
  | 'originalStem'
  | 'extension'
  | 'sequence'
  | 'year'
  | 'month'
  | 'day'
  | 'hour'
  | 'minute'
  | 'make'
  | 'model'
  | 'lensModel'
  | 'iso'
  | 'artist'
  | 'copyright'
  | 'imageDescription'
  | 'keywords'
  | 'rating'
  | 'colorLabel'
  | 'headline'
  | 'location';

export interface MetadataValue {
  token: MetadataToken;
  rawValue: string | null;
  normalizedValue: string | null;
  source: string | null;
  missing: boolean;
}

export type PatternPart =
  { type: 'literal'; value: string } | { type: 'token'; token: MetadataToken; fallback: string | null };

export interface ImportPattern {
  version: number;
  parts: PatternPart[];
  missingTokenPolicy: MissingTokenPolicy;
}

export interface ImportPlanRequest {
  sourceFiles: string[];
  sourceFolder: string | null;
  recursive: boolean;
  destinationRoot: string;
  folderPattern: ImportPattern;
  filenamePattern: ImportPattern;
  operation: ImportOperation;
  collisionPolicy: CollisionPolicy;
  includeAssociatedFiles: boolean;
  preserveTimestamps?: boolean;
  useCaptureTime?: boolean;
}

export type ImportPreviewStatus = 'ready' | 'renamed' | 'skipped' | 'blocked';

export interface ImportConflict {
  code: string;
  message: string;
  conflictingSource: string | null;
}

export interface ImportPreviewRow {
  sourcePath: string;
  destinationRelativePath: string;
  metadata: Partial<Record<MetadataToken, MetadataValue>>;
  associatedFiles: string[];
  byteSize: number | null;
  status: ImportPreviewStatus;
  conflicts: ImportConflict[];
  warnings: string[];
}

export interface ImportPreview {
  planId: string;
  requestHash: string;
  rows: ImportPreviewRow[];
  page: number;
  pageSize: number;
  totalRows: number;
  hasMore: boolean;
  unsupportedFileCount: number;
  warnings: string[];
  errors: string[];
}

export interface ExecuteImportRequest {
  planId: string;
  requestHash: string;
}

export interface ImportPreviewPageRequest {
  planId: string;
  page: number;
  pageSize: number;
}

export interface ImportPreset {
  id: string;
  name: string;
  folderPattern: ImportPattern;
  filenamePattern: ImportPattern;
  operation: ImportOperation;
  collisionPolicy: CollisionPolicy;
  missingTokenPolicy: MissingTokenPolicy;
  includeAssociatedFiles: boolean;
  preserveTimestamps: boolean;
  useCaptureTime: boolean;
  lastSourceLocation?: string | null;
  lastTargetLocation?: string | null;
}

export type ImportPhase = 'scanning' | 'planning' | 'copying' | 'verifying' | 'moving' | 'complete' | 'cancelled';

export interface ImportProgressEvent {
  planId: string | null;
  phase: ImportPhase;
  current: number;
  total: number;
  bytesCompleted: number | null;
  bytesTotal: number | null;
  sourcePath: string | null;
}

export type ImportItemStatus = 'succeeded' | 'renamed' | 'skipped' | 'warned' | 'failed' | 'cancelled';

export interface ImportItemResult {
  sourcePath: string;
  destinationRelativePath: string | null;
  status: ImportItemStatus;
  sourceRetained: boolean;
  warnings: string[];
  error: string | null;
}

export interface ImportResult {
  planId: string;
  cancelled: boolean;
  succeeded: number;
  renamed: number;
  skipped: number;
  warned: number;
  failed: number;
  retainedSources: number;
  items: ImportItemResult[];
}

export type WorkflowLanguage = 'python' | 'javaScript';
export type WorkflowPhase = 'postImage' | 'postBatch';
export type WorkflowSource = 'bundled' | 'user';
export type WorkflowErrorPolicy = 'warn' | 'fail';

export interface WorkflowRuntime {
  available: boolean;
  executable: string | null;
  version: string | null;
  unavailableReason: string | null;
}

export interface WorkflowDiagnostic {
  code: string;
  message: string;
}

export interface DiscoveredWorkflow {
  id: string;
  displayName: string;
  description: string | null;
  language: WorkflowLanguage;
  phase: WorkflowPhase;
  order: number;
  timeoutSeconds: number;
  onError: WorkflowErrorPolicy;
  source: WorkflowSource;
  /** Canonical script location; never a symlink and always inside its scanning root. */
  scriptPath: string;
  /** False when invalid metadata makes the workflow unselectable; runtime availability is separate. */
  selectable: boolean;
  runtime: WorkflowRuntime;
  diagnostics: WorkflowDiagnostic[];
}

export interface WorkflowDiscoveryResult {
  workflows: DiscoveredWorkflow[];
  diagnostics: WorkflowDiagnostic[];
}

/** Full discovery state plus the delta against the previously cached run. */
export interface WorkflowRefreshReport {
  result: WorkflowDiscoveryResult;
  addedWorkflowIds: string[];
  removedWorkflowIds: string[];
  availabilityChanges: string[];
}

export interface WorkflowSelection {
  workflowId: string;
}

/**
 * Optional `<script>.rapidraw.json` sidecar. A direct `.py`/`.js` file with no
 * sidecar is valid; every field may be omitted and safe defaults apply.
 */
export interface WorkflowMetadata {
  id?: string;
  displayName?: string;
  description?: string;
  phase?: WorkflowPhase;
  order?: number;
  timeoutSeconds?: number;
  onError?: WorkflowErrorPolicy;
}

export interface WorkflowArtifact {
  path: string;
  kind: string | null;
}

export interface WorkflowItem {
  sourcePath: string;
  exportedPath: string | null;
  artifacts: WorkflowArtifact[];
  error: string | null;
}

export interface WorkflowExportSettings {
  fileFormat: string;
  jpegQuality: number;
  keepMetadata: boolean;
  stripGps: boolean;
}

export interface WorkflowRequest {
  protocolVersion: typeof WORKFLOW_PROTOCOL_VERSION;
  runId: string;
  workflowId: string;
  phase: WorkflowPhase;
  sourcePath: string | null;
  exportedPath: string | null;
  artifacts: WorkflowArtifact[];
  selectedItems: WorkflowItem[];
  exportedItems: WorkflowItem[];
  exportSettings: WorkflowExportSettings;
  index: number | null;
  total: number;
  workspaceTempDirectory: string;
}

export interface WorkflowResponse {
  ok: boolean;
  message: string | null;
  warnings: string[];
  producedArtifactPaths: string[];
}

export type WorkflowProgressPhase =
  | 'discovering'
  | 'waiting'
  | 'rendering'
  | 'writing'
  | 'runningPostImage'
  | 'runningPostBatch'
  | 'cancelling'
  | 'complete'
  | 'cancelled';

export interface WorkflowProgressEvent {
  runId: string;
  phase: WorkflowProgressPhase;
  workflowId: string | null;
  sourcePath: string | null;
  index: number;
  total: number;
  timeoutSeconds: number | null;
  warningCount: number;
}

export type WorkflowRunStatus = 'succeeded' | 'warned' | 'failed' | 'cancelled' | 'timedOut';

export interface WorkflowRunResult {
  workflowId: string;
  sourcePath: string | null;
  status: WorkflowRunStatus;
  message: string | null;
  warnings: string[];
  producedArtifacts: WorkflowArtifact[];
  /** Bounded, redacted workflow stderr excerpt; never raw or unbounded. */
  stderrExcerpt: string | null;
}

export interface WorkflowBatchResult {
  runId: string;
  cancelled: boolean;
  results: WorkflowRunResult[];
}

export interface ExportSettings {
  filenameTemplate: string | null;
  jpegQuality: number;
  keepMetadata: boolean;
  preserveTimestamps: boolean;
  resize: {
    mode: string;
    value: number;
    dontEnlarge: boolean;
  } | null;
  stripGps: boolean;
  watermark: WatermarkSettings | null;
  exportMasks?: boolean;
  preserveFolders?: boolean;
  workflowIds?: string[];
}

export enum WatermarkAnchor {
  TopLeft = 'topLeft',
  TopCenter = 'topCenter',
  TopRight = 'topRight',
  CenterLeft = 'centerLeft',
  Center = 'center',
  CenterRight = 'centerRight',
  BottomLeft = 'bottomLeft',
  BottomCenter = 'bottomCenter',
  BottomRight = 'bottomRight',
}

interface WatermarkSettings {
  path: string;
  anchor: WatermarkAnchor;
  scale: number;
  spacing: number;
  opacity: number;
}

/** One settled image of an export: exported path on success, error on failure. */
export interface ExportItemOutcome {
  sourcePath: string;
  exportedPath: string | null;
  error: string | null;
}

/** Terminal export detail: per-image and per-workflow outcomes with counts. */
export interface ExportResultDetail {
  runId: string;
  cancelled: boolean;
  total: number;
  succeeded: number;
  failed: number;
  warnedWorkflowRuns: number;
  failedWorkflowRuns: number;
  items: ExportItemOutcome[];
  workflowRuns: WorkflowRunResult[];
}

export interface ExportState {
  errorMessage: string;
  progress: Progress;
  status: Status;
  /** Latest typed workflow lifecycle event of the current/latest run. */
  workflow?: WorkflowProgressEvent | null;
  /** Terminal detail of the latest export; inspectable until dismissed. */
  result?: ExportResultDetail | null;
}

export interface FileFormat {
  extensions: Array<string>;
  id: string;
  name: string;
}

export interface ImportState {
  errorMessage: string;
  path?: string;
  progress?: Progress;
  status: Status;
  phase?: ImportPhase;
  planId?: string | null;
  bytesCompleted?: number | null;
  bytesTotal?: number | null;
  result?: ImportResult | null;
}

export enum Status {
  Cancelled = 'cancelled',
  Cancelling = 'cancelling',
  Exporting = 'exporting',
  Error = 'error',
  Idle = 'idle',
  Importing = 'importing',
  Success = 'success',
}

export interface ExportPreset {
  id: string;
  name: string;
  fileFormat: string;
  jpegQuality: number;
  enableResize: boolean;
  resizeMode: string;
  resizeValue: number;
  dontEnlarge: boolean;
  keepMetadata: boolean;
  preserveTimestamps: boolean;
  stripGps: boolean;
  exportMasks?: boolean;
  preserveFolders?: boolean;
  filenameTemplate: string;
  enableWatermark: boolean;
  watermarkPath: string | null;
  watermarkAnchor: string;
  watermarkScale: number;
  watermarkSpacing: number;
  watermarkOpacity: number;
  workflowIds?: string[];
  lastExportPath?: string;
}
