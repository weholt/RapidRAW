import { act, fireEvent, render, renderHook, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { DiscoveredWorkflow } from '../ui/ExportImportProperties';
import ExportWorkflowSelect, { useExportWorkflowDiscovery, type WorkflowDiscoveryState } from './ExportWorkflowSelect';

const invoke = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invoke(...args),
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

function discoveryState(overrides: Partial<WorkflowDiscoveryState> = {}): WorkflowDiscoveryState {
  return {
    status: 'ready',
    workflows: [],
    errorMessage: null,
    refresh: vi.fn(),
    ...overrides,
  };
}

function renderSelect(
  selectedIds: string[],
  onChange: (ids: string[]) => void,
  overrides: Partial<WorkflowDiscoveryState> = {},
) {
  const discovery = discoveryState(overrides);
  return render(<ExportWorkflowSelect discovery={discovery} selectedIds={selectedIds} onChange={onChange} />);
}

describe('ExportWorkflowSelect states', () => {
  it('shows the loading state before discovery finishes', () => {
    renderSelect([], vi.fn(), { status: 'loading', workflows: [] });
    expect(screen.getByText('export.workflows.loading')).toBeInTheDocument();
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
  });

  it('shows an error message when discovery fails without results', () => {
    renderSelect([], vi.fn(), { status: 'error', errorMessage: 'scan failed', workflows: [] });
    expect(screen.getByText('export.workflows.error')).toBeInTheDocument();
    expect(screen.getByText('scan failed')).toBeInTheDocument();
  });

  it('renders a hint when no workflows are discovered', () => {
    renderSelect([], vi.fn(), { status: 'ready', workflows: [] });
    expect(screen.getByText('export.workflows.noWorkflows')).toBeInTheDocument();
  });

  it('triggers an explicit refresh on demand', () => {
    const refresh = vi.fn();
    renderSelect([], vi.fn(), { refresh });
    fireEvent.click(screen.getByRole('button', { name: 'export.workflows.refresh' }));
    expect(refresh).toHaveBeenCalledTimes(1);
  });
});

describe('ExportWorkflowSelect workflow entries', () => {
  const receipt = workflow();
  const backup = workflow({
    id: 'backup',
    displayName: 'Backup',
    language: 'python',
    phase: 'postImage',
    source: 'bundled',
    scriptPath: 'C:/program/resources/workflows/backup.py',
    runtime: {
      available: true,
      executable: 'py',
      version: 'Python 3.12.0',
      unavailableReason: null,
    },
  });

  it('lists workflows with language, runtime version, phase, and source', () => {
    renderSelect([], vi.fn(), { workflows: [receipt, backup] });
    expect(screen.getByRole('checkbox', { name: /Receipt/ })).toBeEnabled();
    expect(screen.getByRole('checkbox', { name: /Backup/ })).toBeEnabled();
    expect(screen.getByText('export.workflows.languageJavaScript v24.1.0')).toBeInTheDocument();
    expect(screen.getByText('export.workflows.phasePostBatch')).toBeInTheDocument();
    expect(screen.getByText('export.workflows.sourceUser')).toBeInTheDocument();
    expect(screen.getByText('export.workflows.languagePython Python 3.12.0')).toBeInTheDocument();
    expect(screen.getByText('export.workflows.phasePostImage')).toBeInTheDocument();
    expect(screen.getByText('export.workflows.sourceBundled')).toBeInTheDocument();
  });

  it('toggles workflows on and off by stable id', () => {
    const onChange = vi.fn();
    const { rerender } = renderSelect(['receipt'], onChange, { workflows: [receipt, backup] });
    fireEvent.click(screen.getByRole('checkbox', { name: /Backup/ }));
    expect(onChange).toHaveBeenCalledWith(['receipt', 'backup']);

    rerender(
      <ExportWorkflowSelect
        discovery={discoveryState({ workflows: [receipt, backup] })}
        selectedIds={['receipt', 'backup']}
        onChange={onChange}
      />,
    );
    fireEvent.click(screen.getByRole('checkbox', { name: /Backup/ }));
    expect(onChange).toHaveBeenCalledWith(['receipt']);
  });

  it('keeps missing selected ids visible as unavailable entries', () => {
    const onChange = vi.fn();
    renderSelect(['ghost'], onChange, { workflows: [receipt] });
    expect(screen.getByText('export.workflows.missingBadge')).toBeInTheDocument();
    expect(screen.getByText('export.workflows.reasonMissing')).toBeInTheDocument();
    expect(screen.getByText('ghost')).toBeInTheDocument();
    expect(screen.getByRole('checkbox', { name: /ghost/ })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'export.workflows.remove ghost' }));
    expect(onChange).toHaveBeenCalledWith([]);
  });

  it('disables invalid workflows with an actionable reason', () => {
    const broken = workflow({
      id: 'broken',
      displayName: 'Broken',
      selectable: false,
      diagnostics: [{ code: 'workflow.metadata.order.outOfRange', message: 'order must be 0..=1000' }],
    });
    renderSelect([], vi.fn(), { workflows: [broken] });
    const checkbox = screen.getByRole('checkbox', { name: /Broken/ });
    expect(checkbox).toBeDisabled();
    expect(screen.getByText('export.workflows.reasonInvalid', { exact: false })).toBeInTheDocument();
    expect(screen.getByText('order must be 0..=1000')).toBeInTheDocument();
  });

  it('disables workflows whose runtime is unavailable with the runtime reason', () => {
    const noRuntime = workflow({
      id: 'no-node',
      displayName: 'No Node',
      runtime: {
        available: false,
        executable: null,
        version: null,
        unavailableReason: "'node': could not launch 'node'",
      },
    });
    renderSelect([], vi.fn(), { workflows: [noRuntime] });
    expect(screen.getByRole('checkbox', { name: /No Node/ })).toBeDisabled();
    expect(screen.getByText('export.workflows.reasonUnavailable', { exact: false })).toBeInTheDocument();
    expect(screen.getByText("'node': could not launch 'node'")).toBeInTheDocument();
  });

  it('filters entries by search text over name and id', () => {
    renderSelect([], vi.fn(), { workflows: [receipt, backup] });
    fireEvent.change(screen.getByPlaceholderText('export.workflows.searchPlaceholder'), {
      target: { value: 'rece' },
    });
    expect(screen.getByRole('checkbox', { name: /Receipt/ })).toBeInTheDocument();
    expect(screen.queryByRole('checkbox', { name: /Backup/ })).not.toBeInTheDocument();

    fireEvent.change(screen.getByPlaceholderText('export.workflows.searchPlaceholder'), {
      target: { value: 'BACKUP' },
    });
    expect(screen.getByRole('checkbox', { name: /Backup/ })).toBeInTheDocument();
    expect(screen.queryByRole('checkbox', { name: /Receipt/ })).not.toBeInTheDocument();
  });
});

describe('ExportWorkflowSelect ordering', () => {
  const receipt = workflow();
  const backup = workflow({
    id: 'backup',
    displayName: 'Backup',
    scriptPath: 'C:/Users/photographer/.rapidraw/workflows/backup.js',
  });

  it('moves selected workflows up and down', () => {
    const onChange = vi.fn();
    renderSelect(['receipt', 'backup'], onChange, { workflows: [receipt, backup] });
    fireEvent.click(screen.getByRole('button', { name: 'export.workflows.moveUp Backup' }));
    expect(onChange).toHaveBeenCalledWith(['backup', 'receipt']);
    fireEvent.click(screen.getByRole('button', { name: 'export.workflows.moveDown Receipt' }));
    expect(onChange).toHaveBeenCalledWith(['backup', 'receipt']);
  });

  it('disables boundary move buttons and removes selections', () => {
    const onChange = vi.fn();
    renderSelect(['receipt', 'backup'], onChange, { workflows: [receipt, backup] });
    expect(screen.getByRole('button', { name: 'export.workflows.moveUp Receipt' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'export.workflows.moveDown Backup' })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'export.workflows.remove Receipt' }));
    expect(onChange).toHaveBeenCalledWith(['backup']);
  });
});

describe('useExportWorkflowDiscovery', () => {
  beforeEach(() => {
    invoke.mockReset();
  });

  it('starts idle and never discovers while disabled', () => {
    invoke.mockResolvedValue({ workflows: [], diagnostics: [] });
    const { result } = renderHook(() => useExportWorkflowDiscovery(false));
    expect(result.current.status).toBe('idle');
    expect(invoke).not.toHaveBeenCalled();
  });

  it('discovers workflows when enabled', async () => {
    const receipt = workflow();
    invoke.mockResolvedValue({ workflows: [receipt], diagnostics: [] });
    const { result } = renderHook(() => useExportWorkflowDiscovery(true));
    await waitFor(() => expect(result.current.status).toBe('ready'));
    expect(invoke).toHaveBeenCalledWith('discover_export_workflows');
    expect(result.current.workflows).toEqual([receipt]);
  });

  it('refreshes through the refresh command and applies the report', async () => {
    const receipt = workflow();
    const backup = workflow({
      id: 'backup',
      displayName: 'Backup',
      scriptPath: 'C:/Users/photographer/.rapidraw/workflows/backup.js',
    });
    invoke.mockImplementation((command: string) => {
      if (command === 'discover_export_workflows') {
        return Promise.resolve({ workflows: [receipt], diagnostics: [] });
      }
      if (command === 'refresh_export_workflows') {
        return Promise.resolve({
          result: { workflows: [receipt, backup], diagnostics: [] },
          addedWorkflowIds: ['backup'],
          removedWorkflowIds: [],
          availabilityChanges: [],
        });
      }
      return Promise.resolve(undefined);
    });
    const { result } = renderHook(() => useExportWorkflowDiscovery(true));
    await waitFor(() => expect(result.current.status).toBe('ready'));
    await act(async () => {
      result.current.refresh();
    });
    expect(invoke).toHaveBeenCalledWith('refresh_export_workflows');
    expect(result.current.workflows.map((entry) => entry.id)).toEqual(['receipt', 'backup']);
  });

  it('reports discovery errors', async () => {
    invoke.mockRejectedValue('scan failed');
    const { result } = renderHook(() => useExportWorkflowDiscovery(true));
    await waitFor(() => expect(result.current.status).toBe('error'));
    expect(result.current.errorMessage).toBe('scan failed');
  });
});
