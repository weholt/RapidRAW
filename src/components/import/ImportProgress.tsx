import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { Invokes } from '../ui/AppProperties';
import { useProcessStore } from '../../store/useProcessStore';

export default function ImportProgress() {
  const { t } = useTranslation();
  const state = useProcessStore((value) => value.importState);
  if (!state.phase || ['complete', 'cancelled'].includes(state.phase)) return null;
  return (
    <div role="status" className="space-y-1">
      <p>{t('modals.importSettings.progressPhase', { phase: state.phase })}</p>
      <progress value={state.progress?.current ?? 0} max={state.progress?.total || 1} />
      <button type="button" onClick={() => invoke(Invokes.CancelImport, { planId: state.planId ?? null })}>
        {t('modals.importSettings.cancel')}
      </button>
    </div>
  );
}
