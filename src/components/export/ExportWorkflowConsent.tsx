import { useTranslation } from 'react-i18next';
import { AlertTriangle } from 'lucide-react';
import Text from '../ui/Text';
import Button from '../ui/Button';
import { TextColors, TextVariants } from '../../types/typography';

/**
 * Trust consent for export workflows (rapidraw-060.9).
 *
 * Workflows are trusted local code executed with full user permissions. The
 * first export that selects at least one workflow must surface this warning
 * and record explicit consent before any workflow subprocess is spawned. The
 * record is per-installation, versioned, and stored in localStorage; headless
 * exports are unaffected (they never run unselected workflows and require an
 * explicit `--workflow` flag).
 */

const CONSENT_STORAGE_KEY = 'rapidraw.workflowConsent.v1';
const CONSENT_VERSION = 1;

export interface WorkflowConsentRecord {
  version: number;
  acceptedAt: string;
}

export function readWorkflowConsent(): WorkflowConsentRecord | null {
  try {
    const raw = window.localStorage.getItem(CONSENT_STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<WorkflowConsentRecord>;
    if (parsed.version !== CONSENT_VERSION || typeof parsed.acceptedAt !== 'string') {
      return null;
    }
    return { version: CONSENT_VERSION, acceptedAt: parsed.acceptedAt };
  } catch {
    return null;
  }
}

export function recordWorkflowConsent(): WorkflowConsentRecord {
  const record: WorkflowConsentRecord = {
    version: CONSENT_VERSION,
    acceptedAt: new Date().toISOString(),
  };
  try {
    window.localStorage.setItem(CONSENT_STORAGE_KEY, JSON.stringify(record));
  } catch {
    // Storage unavailable: consent cannot persist, so the warning will be
    // shown again before the next workflow execution. Fail safe, not silent.
  }
  return record;
}

export function hasWorkflowConsent(): boolean {
  return readWorkflowConsent() !== null;
}

interface ExportWorkflowConsentProps {
  open: boolean;
  /** Number of workflows selected for the pending export. */
  workflowCount: number;
  onAccept: () => void;
  onDecline: () => void;
}

export default function ExportWorkflowConsent({
  open,
  workflowCount,
  onAccept,
  onDecline,
}: ExportWorkflowConsentProps) {
  const { t } = useTranslation();
  if (!open) return null;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={t('export.workflows.consent.title')}
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
      data-testid="export-workflow-consent"
    >
      <div className="bg-bg-secondary border border-surface rounded-xl max-w-lg w-full p-4 space-y-3">
        <div className="flex items-center gap-2">
          <AlertTriangle size={18} className="text-yellow-500 shrink-0" />
          <Text variant={TextVariants.title} color={TextColors.primary}>
            {t('export.workflows.consent.title')}
          </Text>
        </div>
        <Text as="div" variant={TextVariants.small} color={TextColors.secondary}>
          {t('export.workflows.consent.intro', { selected: workflowCount })}
        </Text>
        <ul className="space-y-1.5">
          {(['fullPermissions', 'noSandbox', 'onlyTrustedSources', 'discoveryLocations'] as const).map((key) => (
            <li key={key} className="flex gap-2">
              <span className="text-yellow-500 shrink-0" aria-hidden>
                •
              </span>
              <Text as="span" variant={TextVariants.small} color={TextColors.secondary}>
                {t(`export.workflows.consent.${key}`)}
              </Text>
            </li>
          ))}
        </ul>
        <div className="flex justify-end gap-2 pt-1">
          <Button variant="secondary" onClick={onDecline}>
            {t('export.workflows.consent.decline')}
          </Button>
          <Button onClick={onAccept}>{t('export.workflows.consent.accept')}</Button>
        </div>
      </div>
    </div>
  );
}
