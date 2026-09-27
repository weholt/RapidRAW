import { fireEvent, render, renderHook, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { Status, type ImportResult } from '../ui/ExportImportProperties';
import { useProcessStore } from '../../store/useProcessStore';
import { useLibraryStore } from '../../store/useLibraryStore';
import { useTauriListeners } from '../../hooks/useTauriListeners';
import ImportProgress from './ImportProgress';
import ImportResultPanel from './ImportResult';

const { handlers, invoke, listen, writeText } = vi.hoisted(() => {
  const handlers: Record<string, (event: { payload: unknown }) => void> = {};
  const invoke = vi.fn().mockResolvedValue(undefined);
  const listen = vi.fn((name: string, handler: (event: unknown) => void) => {
    handlers[name] = handler as (event: { payload: unknown }) => void;
    return Promise.resolve(() => undefined);
  });
  const writeText = vi.fn().mockResolvedValue(undefined);
  return { handlers, invoke, listen, writeText };
});

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  convertFileSrc: (path: string) => path,
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: (name: string, handler: (event: unknown) => void) => listen(name, handler),
}));

const mixed: ImportResult = {
  planId: 'plan-1',
  cancelled: false,
  succeeded: 1,
  renamed: 0,
  skipped: 0,
  warned: 1,
  failed: 1,
  retainedSources: 1,
  items: [
    {
      sourcePath: 'C:/src/a.nef',
      destinationRelativePath: '2026/a.nef',
      status: 'succeeded',
      sourceRetained: false,
      warnings: [],
      error: null,
    },
    {
      sourcePath: 'C:/src/b.nef',
      destinationRelativePath: '2026/b.nef',
      status: 'failed',
      sourceRetained: true,
      warnings: [],
      error: 'Destination appeared after preview',
    },
    {
      sourcePath: 'C:/src/c.nef',
      destinationRelativePath: '2026/c.nef',
      status: 'warned',
      sourceRetained: true,
      warnings: ['missing metadata fallback'],
      error: null,
    },
  ],
};

describe('import progress and result reporting', () => {
  beforeEach(() => {
    invoke.mockClear();
    writeText.mockClear();
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText },
      configurable: true,
    });
    useProcessStore.setState({
      importState: { errorMessage: '', path: '', progress: { current: 0, total: 0 }, status: Status.Idle },
    });
    useLibraryStore.setState({ currentFolderPath: null });
  });

  it('shows the typed phase, counts, and cancel control while importing', async () => {
    const user = userEvent.setup();
    useProcessStore.getState().setImportState({
      status: Status.Importing,
      phase: 'copying',
      planId: 'plan-1',
      path: 'C:/src/a.nef',
      progress: { current: 2, total: 5 },
      bytesCompleted: 2048,
      bytesTotal: 8192,
    });
    render(<ImportProgress />);
    expect(screen.getByRole('status')).toBeInTheDocument();
    const bar = screen.getByRole('progressbar');
    expect(bar).toHaveAttribute('max', '5');
    expect(bar).toHaveAttribute('value', '2');
    await user.click(screen.getByRole('button', { name: 'modals.importSettings.cancel' }));
    expect(invoke).toHaveBeenCalledWith('cancel_import', { planId: 'plan-1' });
  });

  it('hides progress for terminal phases', () => {
    useProcessStore.getState().setImportState({ status: Status.Success, phase: 'complete' });
    const { container, rerender } = render(<ImportProgress />);
    expect(container).toBeEmptyDOMElement();
    useProcessStore.getState().setImportState({ status: Status.Cancelled, phase: 'cancelled' });
    rerender(<ImportProgress />);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders a structured mixed-result summary that stays inspectable and copyable', async () => {
    useProcessStore.getState().setImportState({ status: Status.Success, result: mixed });
    const { container } = render(<ImportResultPanel />);
    // jsdom does not apply the disclosure activation behavior, so open the details directly.
    const details = container.querySelector('details');
    expect(details).not.toBeNull();
    if (details) details.open = true;
    expect(screen.getByText(/2026\/b\.nef: Destination appeared after preview/)).toBeInTheDocument();
    expect(screen.getByText(/2026\/c\.nef: missing metadata fallback/)).toBeInTheDocument();
    expect(screen.queryByText(/2026\/a\.nef/)).not.toBeInTheDocument();
    expect((navigator as unknown as { clipboard?: { writeText?: unknown } }).clipboard?.writeText).toBe(writeText);
    fireEvent.click(screen.getByRole('button', { name: 'modals.importSettings.copyResult' }));
    await waitFor(() => expect(writeText).toHaveBeenCalledTimes(1));
    const summary = writeText.mock.calls[0][0] as string;
    expect(JSON.parse(summary)).toEqual(mixed);
  });

  it('drives store updates and exactly one terminal library refresh per plan from typed events', async () => {
    const refreshAllFolderTrees = vi.fn();
    const handleSelectSubfolder = vi.fn();
    renderHook(() =>
      useTauriListeners({
        refreshAllFolderTrees,
        handleSelectSubfolder,
        refreshImageList: vi.fn(),
        markGenerated: vi.fn(),
      }),
    );
    await waitFor(() => expect(handlers['import-progress']).toBeDefined());
    useLibraryStore.setState({ currentFolderPath: 'C:/library' });

    handlers['import-progress']({
      payload: {
        planId: 'plan-9',
        phase: 'copying',
        current: 1,
        total: 3,
        bytesCompleted: 10,
        bytesTotal: 30,
        sourcePath: 'C:/src/a.nef',
      },
    });
    const state = useProcessStore.getState().importState;
    expect(state.phase).toBe('copying');
    expect(state.path).toBe('C:/src/a.nef');
    expect(state.progress).toEqual({ current: 1, total: 3 });
    expect(state.bytesTotal).toBe(30);
    expect(state.status).toBe(Status.Idle);

    handlers['import-progress']({
      payload: { planId: 'plan-9', phase: 'complete', current: 3, total: 3 },
    });
    await waitFor(() => expect(refreshAllFolderTrees).toHaveBeenCalledTimes(1));
    expect(handleSelectSubfolder).toHaveBeenCalledWith('C:/library', false);
    expect(useProcessStore.getState().importState.status).toBe(Status.Idle);

    handlers['import-progress']({
      payload: { planId: 'plan-9', phase: 'cancelled', current: 3, total: 3 },
    });
    expect(refreshAllFolderTrees).toHaveBeenCalledTimes(1);
    expect(handleSelectSubfolder).toHaveBeenCalledTimes(1);
    expect(useProcessStore.getState().importState.status).toBe(Status.Cancelled);

    handlers['import-progress']({
      payload: { planId: 'plan-10', phase: 'cancelled', current: 0, total: 0 },
    });
    await waitFor(() => expect(refreshAllFolderTrees).toHaveBeenCalledTimes(2));
  });
});
