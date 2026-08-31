import { fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ExportWorkflowConsent, {
  hasWorkflowConsent,
  readWorkflowConsent,
  recordWorkflowConsent,
} from './ExportWorkflowConsent';

describe('ExportWorkflowConsent persistence', () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  it('starts without consent', () => {
    expect(hasWorkflowConsent()).toBe(false);
    expect(readWorkflowConsent()).toBeNull();
  });

  it('records a versioned, timestamped consent', () => {
    const record = recordWorkflowConsent();
    expect(record.version).toBe(1);
    expect(record.acceptedAt).toBeTruthy();
    expect(hasWorkflowConsent()).toBe(true);
    expect(readWorkflowConsent()).toEqual(record);
  });

  it('rejects malformed stored consent', () => {
    window.localStorage.setItem('rapidraw.workflowConsent.v1', '{not json');
    expect(hasWorkflowConsent()).toBe(false);
    window.localStorage.setItem('rapidraw.workflowConsent.v1', JSON.stringify({ version: 99 }));
    expect(hasWorkflowConsent()).toBe(false);
  });
});

describe('ExportWorkflowConsent dialog', () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  it('renders nothing when closed', () => {
    const { container } = render(
      <ExportWorkflowConsent open={false} workflowCount={2} onAccept={vi.fn()} onDecline={vi.fn()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it('shows the trust warning and reports the decision', () => {
    const onAccept = vi.fn();
    const onDecline = vi.fn();
    render(<ExportWorkflowConsent open workflowCount={3} onAccept={onAccept} onDecline={onDecline} />);

    const dialog = screen.getByRole('dialog');
    expect(dialog).toHaveAttribute('aria-modal', 'true');
    expect(screen.getByText('export.workflows.consent.title')).toBeInTheDocument();
    expect(screen.getByText('export.workflows.consent.intro')).toBeInTheDocument();
    expect(screen.getByText('export.workflows.consent.fullPermissions')).toBeInTheDocument();
    expect(screen.getByText('export.workflows.consent.noSandbox')).toBeInTheDocument();
    expect(screen.getByText('export.workflows.consent.onlyTrustedSources')).toBeInTheDocument();
    expect(screen.getByText('export.workflows.consent.discoveryLocations')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'export.workflows.consent.accept' }));
    expect(onAccept).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole('button', { name: 'export.workflows.consent.decline' }));
    expect(onDecline).toHaveBeenCalledTimes(1);
  });
});
