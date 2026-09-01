import { useTranslation } from 'react-i18next';
import { AlertTriangle, Ban, CheckCircle, XCircle } from 'lucide-react';
import Text from '../ui/Text';
import Button from '../ui/Button';
import { TextColors, TextVariants } from '../../types/typography';
import type { ExportResultDetail, WorkflowRunStatus } from '../ui/ExportImportProperties';

interface ExportResultProps {
  result: ExportResultDetail;
  onDismiss: () => void;
}

function statusLabelKey(status: WorkflowRunStatus) {
  switch (status) {
    case 'warned':
      return 'export.result.statusWarned';
    case 'failed':
      return 'export.result.statusFailed';
    case 'cancelled':
      return 'export.result.statusCancelled';
    case 'timedOut':
      return 'export.result.statusTimedOut';
    default:
      return 'export.result.statusSucceeded';
  }
}

function WorkflowRunRow({ run }: { run: ExportResultDetail['workflowRuns'][number] }) {
  const { t } = useTranslation();
  const degraded = run.status === 'failed' || run.status === 'timedOut';

  return (
    <li className="bg-bg-secondary rounded-md p-2 border border-surface space-y-1">
      <div className="flex items-center gap-2 flex-wrap">
        <Text variant={TextVariants.label} color={degraded ? TextColors.error : TextColors.primary}>
          {run.workflowId}
        </Text>
        <Text variant={TextVariants.small} color={degraded ? TextColors.error : TextColors.secondary}>
          {t(statusLabelKey(run.status))}
        </Text>
        {run.sourcePath && (
          <Text variant={TextVariants.small} color={TextColors.secondary}>
            {run.sourcePath}
          </Text>
        )}
      </div>
      {run.message && (
        <Text as="div" variant={TextVariants.small} color={TextColors.secondary}>
          {run.message}
        </Text>
      )}
      {run.warnings.length > 0 && (
        <ul className="space-y-0.5">
          {run.warnings.map((warning) => (
            <li key={warning} className="flex gap-1">
              <Text as="span" variant={TextVariants.small} className="text-yellow-500">
                {t('export.result.warningPrefix')}:
              </Text>
              <Text as="span" variant={TextVariants.small} className="text-yellow-500">
                {warning}
              </Text>
            </li>
          ))}
        </ul>
      )}
      {run.stderrExcerpt && (
        <div>
          <Text variant={TextVariants.small} color={TextColors.secondary}>
            {t('export.result.stderr')}
          </Text>
          <pre className="text-xs text-text-secondary whitespace-pre-wrap break-all max-h-24 overflow-y-auto bg-surface rounded-xs p-1.5">
            {run.stderrExcerpt}
          </pre>
        </div>
      )}
    </li>
  );
}

export default function ExportResult({ result, onDismiss }: ExportResultProps) {
  const { t } = useTranslation();

  const failedItems = result.items.filter((item) => item.error !== null);
  const summaryIcon = result.cancelled ? (
    <Ban size={16} className="text-yellow-500" />
  ) : result.failed > 0 || result.failedWorkflowRuns > 0 ? (
    <XCircle size={16} className="text-red-400" />
  ) : result.warnedWorkflowRuns > 0 ? (
    <AlertTriangle size={16} className="text-yellow-500" />
  ) : (
    <CheckCircle size={16} className="text-green-500" />
  );

  return (
    <div className="bg-surface rounded-xl p-3 space-y-3" data-testid="export-result">
      <div className="flex items-center gap-2 flex-wrap">
        {summaryIcon}
        <Text variant={TextVariants.label} color={TextColors.primary}>
          {t('export.result.summary', { succeeded: result.succeeded, total: result.total })}
        </Text>
        {result.cancelled && (
          <Text variant={TextVariants.small} className="text-yellow-500">
            {t('export.result.cancelled')}
          </Text>
        )}
        {result.warnedWorkflowRuns > 0 && (
          <Text variant={TextVariants.small} className="text-yellow-500">
            {t('export.result.workflowWarnings', { count: result.warnedWorkflowRuns })}
          </Text>
        )}
        {result.failedWorkflowRuns > 0 && (
          <Text variant={TextVariants.small} color={TextColors.error}>
            {t('export.result.workflowFailures', { count: result.failedWorkflowRuns })}
          </Text>
        )}
        <span className="grow" />
        <Button size="sm" variant="secondary" onClick={onDismiss}>
          {t('export.result.dismiss')}
        </Button>
      </div>

      {failedItems.length > 0 && (
        <div className="space-y-1">
          <Text variant={TextVariants.label} color={TextColors.error}>
            {t('export.result.imageErrors')}
          </Text>
          <ul className="space-y-1">
            {failedItems.map((item) => (
              <li key={item.sourcePath} className="bg-bg-secondary rounded-md p-2 border border-surface">
                <Text variant={TextVariants.small} color={TextColors.primary}>
                  {item.sourcePath}
                </Text>
                <Text variant={TextVariants.small} color={TextColors.error}>
                  {item.error}
                </Text>
              </li>
            ))}
          </ul>
        </div>
      )}

      {result.workflowRuns.length > 0 && (
        <div className="space-y-1">
          <Text variant={TextVariants.label} color={TextColors.primary}>
            {t('export.result.workflowRuns')}
          </Text>
          <ul className="space-y-1.5">
            {result.workflowRuns.map((run) => (
              <WorkflowRunRow key={`${run.workflowId}-${run.sourcePath ?? 'batch'}`} run={run} />
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
