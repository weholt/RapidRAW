import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { Status } from '../../ui/ExportImportProperties';
import type { DiscoveredWorkflow } from '../../ui/ExportImportProperties';
import type { SelectedImage } from '../../ui/AppProperties';
import ExportPanel from './ExportPanel';

const invoke = vi.fn();
const save = vi.fn();
const open = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({
  save: (...args: unknown[]) => save(...args),
  open: (...args: unknown[]) => open(...args),
}));

function workflow(overrides: Partial<DiscoveredWorkflow> = {}): DiscoveredWorkflow {
  return {
    id: 'receipt',
    displayName: 'Receipt',
    description: null,
    language: 'javaScript',
    phase: 'postBatch',
    order: 100,
    timeoutSeconds: 60,
    onError: 'warn',
    source: 'user',
    scriptPath: 'C:/Users/photographer/.rapidraw/workflows/receipt.js',
    selectable: true,
    runtime: {
      available: true,
      executable: 'node',
      version: 'v24.1.0',
      unavailableReason: null,
    },
    diagnostics: [],
    ...overrides,
  };
}

const selectedImage: SelectedImage = {
  path: 'C:/library/a.nef',
  width: 3000,
  height: 2000,
  isRaw: true,
  isReady: true,
  thumbnailUrl: '',
  exif: null,
};

function renderPanel(overrides: Record<string, unknown> = {}) {
  return render(
    <ExportPanel
      exportState={{ errorMessage: '', progress: { current: 0, total: 0 }, status: Status.Idle }}
      multiSelectedPaths={[]}
      selectedImage={selectedImage}
      setExportState={vi.fn()}
      appSettings={null}
      onSettingsChange={vi.fn()}
      rootPaths={['C:/library']}
      isVisible
      {...overrides}
    />,
  );
}

function expandAdvanced() {
  fireEvent.click(screen.getByRole('button', { name: /export\.advanced\.title/ }));
}

describe('ExportPanel workflow multiselect integration', () => {
  beforeEach(() => {
    invoke.mockReset();
    save.mockReset();
    window.localStorage.clear();
    (window as unknown as Record<string, unknown>).__TAURI_OS_PLUGIN_INTERNALS__ = {
      platform: 'windows',
    };
    invoke.mockImplementation((command: string) => {
      if (command === 'discover_export_workflows' || command === 'refresh_export_workflows') {
        return Promise.resolve({
          workflows: [workflow()],
          diagnostics: [],
        });
      }
      if (command === 'estimate_export_sizes') return Promise.resolve(12345);
      return Promise.resolve(undefined);
    });
  });

  it('invokes workflow discovery when the panel is open', async () => {
    renderPanel();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('discover_export_workflows'));
  });

  it('shows the searchable workflow multiselect inside advanced settings', async () => {
    renderPanel();
    expandAdvanced();
    expect(screen.getByPlaceholderText('export.workflows.searchPlaceholder')).toBeInTheDocument();
    expect(await screen.findByRole('checkbox', { name: /Receipt/ })).toBeEnabled();
  });

  it('requires workflow consent before the first workflow export', async () => {
    save.mockResolvedValue('C:/out/a_edited.jpg');
    renderPanel();
    expandAdvanced();
    fireEvent.click(await screen.findByRole('checkbox', { name: /Receipt/ }));
    fireEvent.click(screen.getByRole('button', { name: /export\.status\.exportSingle/ }));
    expect(screen.getByTestId('export-workflow-consent')).toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith('export_images', expect.anything());

    fireEvent.click(screen.getByRole('button', { name: 'export.workflows.consent.accept' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('export_images', expect.anything()));
    const call = invoke.mock.calls.find(([command]) => command === 'export_images');
    expect(call?.[1].exportSettings.workflowIds).toEqual(['receipt']);
    expect(JSON.stringify(call?.[1])).not.toContain('scriptPath');
  });

  it('records consent so later workflow exports skip the warning', async () => {
    window.localStorage.setItem(
      'rapidraw.workflowConsent.v1',
      JSON.stringify({ version: 1, acceptedAt: '2026-08-31T00:00:00.000Z' }),
    );
    save.mockResolvedValue('C:/out/a_edited.jpg');
    renderPanel();
    expandAdvanced();
    fireEvent.click(await screen.findByRole('checkbox', { name: /Receipt/ }));
    fireEvent.click(screen.getByRole('button', { name: /export\.status\.exportSingle/ }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('export_images', expect.anything()));
    expect(screen.queryByTestId('export-workflow-consent')).not.toBeInTheDocument();
  });

  it('declining consent keeps the export unstarted', async () => {
    save.mockResolvedValue('C:/out/a_edited.jpg');
    renderPanel();
    expandAdvanced();
    fireEvent.click(await screen.findByRole('checkbox', { name: /Receipt/ }));
    fireEvent.click(screen.getByRole('button', { name: /export\.status\.exportSingle/ }));
    fireEvent.click(screen.getByRole('button', { name: 'export.workflows.consent.decline' }));
    expect(screen.queryByTestId('export-workflow-consent')).not.toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith('export_images', expect.anything());
    expect(save).not.toHaveBeenCalled();
  });

  it('hides the workflow section and skips discovery on Android', async () => {
    (window as unknown as Record<string, unknown>).__TAURI_OS_PLUGIN_INTERNALS__ = {
      platform: 'android',
    };
    renderPanel();
    expandAdvanced();
    expect(screen.queryByPlaceholderText('export.workflows.searchPlaceholder')).not.toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith('discover_export_workflows');
  });
});

describe('ExportPanel workflow progress and result detail', () => {
  beforeEach(() => {
    invoke.mockReset();
    save.mockReset();
    window.localStorage.clear();
    (window as unknown as Record<string, unknown>).__TAURI_OS_PLUGIN_INTERNALS__ = {
      platform: 'windows',
    };
    invoke.mockImplementation((command: string) => {
      if (command === 'discover_export_workflows' || command === 'refresh_export_workflows') {
        return Promise.resolve({ workflows: [workflow()], diagnostics: [] });
      }
      if (command === 'estimate_export_sizes') return Promise.resolve(12345);
      return Promise.resolve(undefined);
    });
  });

  function exportingState(workflow: Record<string, unknown>) {
    return {
      errorMessage: '',
      progress: { current: 1, total: 3 },
      status: Status.Exporting,
      workflow,
      result: null,
    };
  }

  it('labels image rendering distinctly from workflow execution', () => {
    renderPanel({
      exportState: exportingState({
        runId: 'run-1',
        phase: 'rendering',
        workflowId: null,
        sourcePath: 'C:/library/a.nef',
        index: 0,
        total: 3,
        timeoutSeconds: null,
        warningCount: 0,
      }),
    });

    expect(screen.getByText('export.phase.rendering')).toBeInTheDocument();
    expect(screen.queryByText(/export\.phase\.postImage/)).not.toBeInTheDocument();
  });

  it('shows the running workflow with its timeout during workflow execution', () => {
    renderPanel({
      exportState: exportingState({
        runId: 'run-1',
        phase: 'runningPostImage',
        workflowId: 'receipt',
        sourcePath: 'C:/library/a.nef',
        index: 0,
        total: 3,
        timeoutSeconds: 30,
        warningCount: 2,
      }),
    });

    expect(screen.getByText('export.phase.postImage')).toBeInTheDocument();
    expect(screen.getByText('receipt')).toBeInTheDocument();
    expect(screen.getByText('export.phase.timeout')).toBeInTheDocument();
    expect(screen.getByText('export.phase.warnings')).toBeInTheDocument();
  });

  it('renders the result detail view and dismisses through setExportState', () => {
    const setExportState = vi.fn();
    renderPanel({
      setExportState,
      exportState: {
        errorMessage: '',
        progress: { current: 3, total: 3 },
        status: Status.Error,
        workflow: null,
        result: {
          runId: 'run-1',
          cancelled: false,
          total: 3,
          succeeded: 2,
          failed: 1,
          warnedWorkflowRuns: 0,
          failedWorkflowRuns: 0,
          items: [
            {
              sourcePath: 'C:/library/bad.nef',
              exportedPath: null,
              error: 'render failed',
            },
          ],
          workflowRuns: [],
        },
      },
    });

    expect(screen.getByText('C:/library/bad.nef')).toBeInTheDocument();
    expect(screen.getByText('render failed')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'export.result.dismiss' }));
    expect(setExportState).toHaveBeenCalledWith(expect.any(Function));
  });
});
