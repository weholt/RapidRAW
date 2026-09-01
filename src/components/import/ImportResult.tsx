import { useTranslation } from 'react-i18next';
import { useProcessStore } from '../../store/useProcessStore';

export default function ImportResult() {
  const { t } = useTranslation();
  const result = useProcessStore((state) => state.importState.result);
  if (!result) return null;
  const summary = JSON.stringify(result, null, 2);
  return (
    <details className="rounded border border-surface p-2">
      <summary>
        {t('modals.importSettings.resultSummary', {
          succeeded: result.succeeded,
          failed: result.failed,
          retained: result.retainedSources,
        })}
      </summary>
      <button type="button" onClick={() => navigator.clipboard.writeText(summary)}>
        {t('modals.importSettings.copyResult')}
      </button>
      <ul className="max-h-32 overflow-auto text-xs">
        {result.items
          .filter((item) => item.error || item.warnings.length)
          .map((item) => (
            <li key={item.sourcePath}>
              {item.destinationRelativePath ?? item.sourcePath}: {item.error ?? item.warnings.join('; ')}
            </li>
          ))}
      </ul>
    </details>
  );
}
