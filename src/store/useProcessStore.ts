import { create } from 'zustand';
import { Progress } from '../components/ui/AppProperties';
import { ExportState, ImportState, Status, WorkflowProgressEvent } from '../components/ui/ExportImportProperties';

export interface ExternalEditSession {
  source: string;
  output: string;
  format: string;
  jpegQuality: number;
}

interface ProcessState {
  exportState: ExportState;
  importState: ImportState;
  isIndexing: boolean;
  indexingProgress: Progress;
  thumbnails: Record<string, string>;
  mediumThumbnails: Record<string, string>;
  thumbnailProgress: Progress;
  previews: Record<string, { url: string; thumbKey: string; timestamp: number }>;
  aiModelDownloadStatus: string | null;
  copiedFilePaths: Array<string>;
  isCopied: boolean;
  isPasted: boolean;
  initialFileToOpen: string | null;
  externalEditSession: ExternalEditSession | null;

  setProcess: (state: Partial<ProcessState> | ((state: ProcessState) => Partial<ProcessState>)) => void;
  setExportState: (updater: Partial<ExportState> | ((state: ExportState) => Partial<ExportState>)) => void;
  dismissExportResult: () => void;
  setImportState: (updater: Partial<ImportState> | ((state: ImportState) => Partial<ImportState>)) => void;
  setPreview: (path: string, url: string, thumbKey: string) => void;
  clearPreviews: () => void;
}

/**
 * Whether a workflow-progress event may update the export state. Events from
 * any run other than the tracked one are stale, and once the export reached a
 * terminal state no event may alter it anymore; a new export resets the
 * tracked run when it starts.
 */
export function acceptsWorkflowProgressEvent(exportState: ExportState, payload: WorkflowProgressEvent): boolean {
  const live = exportState.status === Status.Exporting || exportState.status === Status.Cancelling;
  if (!live) return false;
  if (exportState.workflow) {
    return exportState.workflow.runId === payload.runId;
  }
  return true;
}

let copyTimeout: ReturnType<typeof setTimeout>;
let pasteTimeout: ReturnType<typeof setTimeout>;
let importStatusTimeout: ReturnType<typeof setTimeout>;

const MAX_PREVIEW_CACHE_SIZE = 10;

export const useProcessStore = create<ProcessState>((set, get) => ({
  exportState: {
    errorMessage: '',
    progress: { current: 0, total: 0 },
    status: Status.Idle,
    workflow: null,
    result: null,
  },
  importState: { errorMessage: '', path: '', progress: { current: 0, total: 0 }, status: Status.Idle },
  isIndexing: false,
  indexingProgress: { current: 0, total: 0 },
  thumbnails: {},
  mediumThumbnails: {},
  thumbnailProgress: { current: 0, total: 0 },
  previews: {},
  aiModelDownloadStatus: null,
  copiedFilePaths: [],
  isCopied: false,
  isPasted: false,
  initialFileToOpen: null,
  externalEditSession: null,

  setProcess: (updater) => {
    set((prev) => {
      const nextState = typeof updater === 'function' ? updater(prev) : updater;
      return { ...prev, ...nextState };
    });

    const state = get();
    if (state.isCopied) {
      clearTimeout(copyTimeout);
      copyTimeout = setTimeout(() => set({ isCopied: false }), 1000);
    }
    if (state.isPasted) {
      clearTimeout(pasteTimeout);
      pasteTimeout = setTimeout(() => set({ isPasted: false }), 1000);
    }
  },

  setExportState: (updater) => {
    set((prev) => ({
      exportState: { ...prev.exportState, ...(typeof updater === 'function' ? updater(prev.exportState) : updater) },
    }));
    // Terminal export details remain inspectable until dismissed or a new
    // export starts; there is no auto-reset.
  },

  dismissExportResult: () => {
    set((prev) => ({
      exportState: {
        ...prev.exportState,
        status: Status.Idle,
        errorMessage: '',
        workflow: null,
        result: null,
        progress: { current: 0, total: 0 },
      },
    }));
  },

  setImportState: (updater) => {
    set((prev) => ({
      importState: { ...prev.importState, ...(typeof updater === 'function' ? updater(prev.importState) : updater) },
    }));

    // The header status indicator is transient: once the import settles, the
    // status resets to idle after a delay so the library header does not show
    // a stale "complete"/"failed" badge forever. The detailed result, phase,
    // and plan id stay inspectable until the next import replaces them.
    const status = get().importState.status;
    clearTimeout(importStatusTimeout);
    if ([Status.Success, Status.Error, Status.Cancelled].includes(status)) {
      importStatusTimeout = setTimeout(() => {
        set((prev) => ({
          importState: {
            ...prev.importState,
            status: Status.Idle,
            errorMessage: '',
            progress: { current: 0, total: 0 },
          },
        }));
      }, 5000);
    }
  },

  setPreview: (path, url, thumbKey) => {
    set((state) => {
      const newPreviews = { ...state.previews };

      if (newPreviews[path] && newPreviews[path].url !== url) {
        URL.revokeObjectURL(newPreviews[path].url);
      }

      newPreviews[path] = { url, thumbKey, timestamp: Date.now() };

      const keys = Object.keys(newPreviews);
      if (keys.length > MAX_PREVIEW_CACHE_SIZE) {
        let oldestPath = keys[0];
        let oldestTime = newPreviews[oldestPath].timestamp;

        for (let i = 1; i < keys.length; i++) {
          const key = keys[i];
          if (newPreviews[key].timestamp < oldestTime) {
            oldestTime = newPreviews[key].timestamp;
            oldestPath = key;
          }
        }

        if (oldestPath !== path) {
          URL.revokeObjectURL(newPreviews[oldestPath].url);
          delete newPreviews[oldestPath];
        }
      }

      return { previews: newPreviews };
    });
  },

  clearPreviews: () => {
    set((state) => {
      Object.values(state.previews).forEach((p) => URL.revokeObjectURL(p.url));
      return { previews: {} };
    });
  },
}));
