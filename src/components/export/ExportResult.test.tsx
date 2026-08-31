import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { ExportResultDetail } from '../ui/ExportImportProperties';
import ExportResult from './ExportResult';

const mixedResult: ExportResultDetail = {
  runId: 'run-1',
  cancelled: false,
  total: 3,
  succeeded: 2,
  failed: 1,
  warnedWorkflowRuns: 1,
  failedWorkflowRuns: 1,
  items: [
    { sourcePath: 'C:/library/ok-a.nef', exportedPath: 'C:/out/ok-a.jpg', error: null },
    { sourcePath: 'C:/library/ok-b.nef', exportedPath: 'C:/out/ok-b.jpg', error: null },
    { sourcePath: 'C:/library/bad.nef', exportedPath: null, error: 'render failed' },
  ],
  workflowRuns: [
    {
      workflowId: 'receipt',
      sourcePath: 'C:/library/ok-a.nef',
      status: 'warned',
      message: 'done with warnings',
      warnings: ['stale sidecar'],
      producedArtifacts: [],
      stderrExcerpt: '~/library/receipt.log',
    },
    {
      workflowId: 'uploader',
      sourcePath: null,
      status: 'failed',
      message: 'bucket unreachable',
      warnings: [],
      producedArtifacts: [],
      stderrExcerpt: null,
    },
  ],
};

describe('ExportResult', () => {
  it('shows the item and workflow counts with per-item errors', () => {
    render(<ExportResult result={mixedResult} onDismiss={vi.fn()} />);

    expect(screen.getByText('export.result.summary')).toBeInTheDocument();
    expect(screen.getByText('C:/library/bad.nef')).toBeInTheDocument();
    expect(screen.getByText('render failed')).toBeInTheDocument();
  });

  it('lists workflow outcomes with warnings and stderr excerpts', () => {
    render(<ExportResult result={mixedResult} onDismiss={vi.fn()} />);

    expect(screen.getByText('receipt')).toBeInTheDocument();
    expect(screen.getByText('export.result.statusWarned')).toBeInTheDocument();
    expect(screen.getByText('stale sidecar')).toBeInTheDocument();
    expect(screen.getByText('~/library/receipt.log')).toBeInTheDocument();

    expect(screen.getByText('uploader')).toBeInTheDocument();
    expect(screen.getByText('export.result.statusFailed')).toBeInTheDocument();
    expect(screen.getByText('bucket unreachable')).toBeInTheDocument();
  });

  it('marks cancelled exports without listing them as workflow failures', () => {
    render(<ExportResult result={{ ...mixedResult, cancelled: true, failedWorkflowRuns: 0 }} onDismiss={vi.fn()} />);

    expect(screen.getByText('export.result.cancelled')).toBeInTheDocument();
  });

  it('invokes onDismiss from the dismiss control', () => {
    const onDismiss = vi.fn();
    render(<ExportResult result={mixedResult} onDismiss={onDismiss} />);

    fireEvent.click(screen.getByRole('button', { name: 'export.result.dismiss' }));

    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it('renders an all-success result without error sections', () => {
    render(
      <ExportResult
        result={{
          runId: 'run-2',
          cancelled: false,
          total: 1,
          succeeded: 1,
          failed: 0,
          warnedWorkflowRuns: 0,
          failedWorkflowRuns: 0,
          items: [{ sourcePath: 'C:/library/ok.nef', exportedPath: 'C:/out/ok.jpg', error: null }],
          workflowRuns: [],
        }}
        onDismiss={vi.fn()}
      />,
    );

    expect(screen.queryByText('export.result.imageErrors')).not.toBeInTheDocument();
    expect(screen.queryByText('export.result.workflowRuns')).not.toBeInTheDocument();
  });
});
