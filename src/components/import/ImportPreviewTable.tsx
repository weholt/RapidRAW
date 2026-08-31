import { useTranslation } from 'react-i18next';
import type { ImportPreview } from '../ui/ExportImportProperties';

interface Props {
  preview: ImportPreview | null;
  loading: boolean;
  onPage(page: number): void;
}

export default function ImportPreviewTable({ preview, loading, onPage }: Props) {
  const { t } = useTranslation();
  if (loading) return <p role="status">{t('modals.importSettings.buildingPreview')}</p>;
  if (!preview) return <p>{t('modals.importSettings.previewPrompt')}</p>;
  return (
    <section aria-label={t('modals.importSettings.preview')} className="space-y-2">
      <div className="max-h-56 overflow-auto rounded border border-surface">
        <table className="w-full text-left text-xs">
          <thead className="sticky top-0 bg-surface">
            <tr>
              <th className="p-2">{t('modals.importSettings.source')}</th>
              <th className="p-2">{t('modals.importSettings.destination')}</th>
              <th className="p-2">{t('modals.importSettings.status')}</th>
            </tr>
          </thead>
          <tbody>
            {preview.rows.map((row) => (
              <tr key={row.sourcePath} className={row.status === 'blocked' ? 'text-red-400' : ''}>
                <td className="max-w-52 truncate p-2" title={row.sourcePath}>
                  {row.sourcePath}
                </td>
                <td className="max-w-52 truncate p-2" title={row.destinationRelativePath}>
                  {row.destinationRelativePath}
                </td>
                <td className="p-2">
                  {row.status}
                  {row.conflicts.map((conflict) => (
                    <div key={conflict.code}>{conflict.message}</div>
                  ))}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="flex items-center justify-between">
        <span>
          {t('modals.importSettings.previewCount', {
            count: preview.totalRows,
            unsupported: preview.unsupportedFileCount,
          })}
        </span>
        <div className="flex gap-2">
          <button type="button" disabled={preview.page === 0} onClick={() => onPage(preview.page - 1)}>
            {t('modals.importSettings.previous')}
          </button>
          <button type="button" disabled={!preview.hasMore} onClick={() => onPage(preview.page + 1)}>
            {t('modals.importSettings.next')}
          </button>
        </div>
      </div>
    </section>
  );
}
