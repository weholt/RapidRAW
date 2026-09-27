import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { acceptsWorkflowProgressEvent, useProcessStore } from './useProcessStore';
import { ExportResultDetail, Status, WorkflowProgressEvent } from '../components/ui/ExportImportProperties';

function resetExportState() {
  useProcessStore.setState({
    exportState: {
      errorMessage: '',
      progress: { current: 0, total: 0 },
      status: Status.Idle,
      workflow: null,
      result: null,
    },
  });
}

const result: ExportResultDetail = {
  runId: 'run-1',
  cancelled: false,
  total: 2,
  succeeded: 1,
  failed: 1,
  warnedWorkflowRuns: 1,
  failedWorkflowRuns: 0,
  items: [
    {
      sourcePath: 'C:/library/a.nef',
      exportedPath: 'C:/out/a.jpg',
      error: null,
    },
    {
      sourcePath: 'C:/library/b.nef',
      exportedPath: null,
      error: 'render failed',
    },
  ],
  workflowRuns: [],
};

describe('useProcessStore export terminal state', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetExportState();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('keeps terminal export state inspectable instead of auto-resetting', () => {
    useProcessStore.getState().setExportState({ status: Status.Error, result });

    vi.advanceTimersByTime(10_000);

    const state = useProcessStore.getState().exportState;
    expect(state.status).toBe(Status.Error);
    expect(state.result).toEqual(result);
  });

  it('dismissExportResult resets the export state to idle', () => {
    useProcessStore.getState().setExportState({ status: Status.Success, errorMessage: '', result });

    useProcessStore.getState().dismissExportResult();

    const state = useProcessStore.getState().exportState;
    expect(state.status).toBe(Status.Idle);
    expect(state.result).toBeNull();
    expect(state.workflow).toBeNull();
    expect(state.errorMessage).toBe('');
    expect(state.progress).toEqual({ current: 0, total: 0 });
  });

  it('accepts workflow progress only while an export is live', () => {
    const event: WorkflowProgressEvent = {
      runId: 'run-1',
      phase: 'runningPostImage',
      workflowId: 'receipt',
      sourcePath: 'C:/library/a.nef',
      index: 0,
      total: 2,
      timeoutSeconds: 30,
      warningCount: 0,
    };

    useProcessStore.getState().setExportState({ status: Status.Exporting });
    expect(acceptsWorkflowProgressEvent(useProcessStore.getState().exportState, event)).toBe(true);

    useProcessStore.getState().setExportState({ workflow: event });
    expect(
      acceptsWorkflowProgressEvent(useProcessStore.getState().exportState, {
        ...event,
        runId: 'run-0',
      }),
    ).toBe(false);

    useProcessStore.getState().setExportState({ status: Status.Success });
    expect(acceptsWorkflowProgressEvent(useProcessStore.getState().exportState, event)).toBe(false);
  });
});
