import { render } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useTauriListeners } from './useTauriListeners';
import { useProcessStore } from '../store/useProcessStore';
import { ExportResultDetail, Status, WorkflowProgressEvent } from '../components/ui/ExportImportProperties';

type EventHandler = (event: { payload: unknown }) => void;
const handlers = new Map<string, EventHandler>();

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((name: string, handler: EventHandler) => {
    handlers.set(name, handler);
    return Promise.resolve(() => handlers.delete(name));
  }),
}));

vi.mock('@tauri-apps/api/core', () => ({
  convertFileSrc: (path: string) => path,
}));

function Harness() {
  useTauriListeners({
    refreshAllFolderTrees: vi.fn(),
    handleSelectSubfolder: vi.fn(),
    refreshImageList: vi.fn(),
    markGenerated: vi.fn(),
  });
  return null;
}

function emit(name: string, payload: unknown) {
  handlers.get(name)?.({ payload });
}

const runningPostImage: WorkflowProgressEvent = {
  runId: 'run-1',
  phase: 'runningPostImage',
  workflowId: 'receipt',
  sourcePath: 'C:/library/a.nef',
  index: 1,
  total: 3,
  timeoutSeconds: 30,
  warningCount: 2,
};

const result: ExportResultDetail = {
  runId: 'run-1',
  cancelled: false,
  total: 3,
  succeeded: 3,
  failed: 0,
  warnedWorkflowRuns: 1,
  failedWorkflowRuns: 0,
  items: [],
  workflowRuns: [],
};

describe('useTauriListeners workflow progress events', () => {
  beforeEach(() => {
    handlers.clear();
    useProcessStore.setState({
      exportState: {
        errorMessage: '',
        progress: { current: 0, total: 0 },
        status: Status.Idle,
        workflow: null,
        result: null,
      },
    });
    render(<Harness />);
  });

  it('records workflow progress while an export is live', () => {
    useProcessStore.getState().setExportState({ status: Status.Exporting });

    emit('workflow-progress', runningPostImage);

    expect(useProcessStore.getState().exportState.workflow).toEqual(runningPostImage);
  });

  it('rejects events from a stale run id', () => {
    useProcessStore.getState().setExportState({
      status: Status.Exporting,
      workflow: { ...runningPostImage, runId: 'run-2' },
    });

    emit('workflow-progress', runningPostImage);

    expect(useProcessStore.getState().exportState.workflow?.runId).toBe('run-2');
  });

  it('rejects events after the export reached a terminal state', () => {
    useProcessStore.getState().setExportState({ status: Status.Cancelled, workflow: null });

    emit('workflow-progress', runningPostImage);

    expect(useProcessStore.getState().exportState.workflow).toBeNull();
  });

  it('stores the terminal export result detail without altering status', () => {
    emit('export-result', result);

    expect(useProcessStore.getState().exportState.result).toEqual(result);
    expect(useProcessStore.getState().exportState.status).toBe(Status.Idle);
  });

  it('does not touch export state for unrelated events', () => {
    useProcessStore.getState().setExportState({ status: Status.Exporting });

    emit('thumbnail-progress', { current: 1, total: 2 });

    expect(useProcessStore.getState().exportState.workflow).toBeNull();
  });
});
