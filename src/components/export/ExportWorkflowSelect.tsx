import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { ChevronDown, ChevronUp, RefreshCw, X } from 'lucide-react';
import Text from '../ui/Text';
import { TextColors, TextVariants } from '../../types/typography';
import { Invokes } from '../ui/AppProperties';
import {
  DiscoveredWorkflow,
  WorkflowDiscoveryResult,
  WorkflowLanguage,
  WorkflowRefreshReport,
} from '../ui/ExportImportProperties';

export type WorkflowDiscoveryStatus = 'idle' | 'loading' | 'ready' | 'error';

export interface WorkflowDiscoveryState {
  status: WorkflowDiscoveryStatus;
  workflows: DiscoveredWorkflow[];
  errorMessage: string | null;
  refresh: () => void;
}

export function useExportWorkflowDiscovery(enabled: boolean): WorkflowDiscoveryState {
  const [status, setStatus] = useState<WorkflowDiscoveryStatus>('idle');
  const [workflows, setWorkflows] = useState<DiscoveredWorkflow[]>([]);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const requestId = useRef(0);
  const started = useRef(false);

  const run = useCallback(async (command: Invokes) => {
    const id = requestId.current + 1;
    requestId.current = id;
    setStatus('loading');
    setErrorMessage(null);
    try {
      const payload =
        command === Invokes.RefreshExportWorkflows
          ? ((await invoke(Invokes.RefreshExportWorkflows)) as WorkflowRefreshReport).result
          : ((await invoke(Invokes.DiscoverExportWorkflows)) as WorkflowDiscoveryResult);
      if (id !== requestId.current) return;
      setWorkflows(payload.workflows);
      setStatus('ready');
    } catch (error) {
      if (id !== requestId.current) return;
      setErrorMessage(typeof error === 'string' ? error : String(error));
      setStatus('error');
    }
  }, []);

  useEffect(() => {
    if (!enabled || started.current) return;
    started.current = true;
    run(Invokes.DiscoverExportWorkflows);
  }, [enabled, run]);

  const refresh = useCallback(() => {
    run(Invokes.RefreshExportWorkflows);
  }, [run]);

  return { status, workflows, errorMessage, refresh };
}

type WorkflowLanguageLabelKey = 'export.workflows.languageJavaScript' | 'export.workflows.languagePython';
type WorkflowPhaseLabelKey = 'export.workflows.phasePostImage' | 'export.workflows.phasePostBatch';
type WorkflowSourceLabelKey = 'export.workflows.sourceBundled' | 'export.workflows.sourceUser';

function languageLabelKey(language: WorkflowLanguage): WorkflowLanguageLabelKey {
  return language === 'javaScript' ? 'export.workflows.languageJavaScript' : 'export.workflows.languagePython';
}

function phaseLabelKey(phase: string): WorkflowPhaseLabelKey {
  return phase === 'postImage' ? 'export.workflows.phasePostImage' : 'export.workflows.phasePostBatch';
}

function sourceLabelKey(source: string): WorkflowSourceLabelKey {
  return source === 'bundled' ? 'export.workflows.sourceBundled' : 'export.workflows.sourceUser';
}

type WorkflowEntry = { kind: 'discovered'; workflow: DiscoveredWorkflow } | { kind: 'missing'; id: string };

interface ExportWorkflowSelectProps {
  discovery: WorkflowDiscoveryState;
  selectedIds: string[];
  onChange: (ids: string[]) => void;
  disabled?: boolean;
}

export default function ExportWorkflowSelect({
  discovery,
  selectedIds,
  onChange,
  disabled = false,
}: ExportWorkflowSelectProps) {
  const { t } = useTranslation();
  const [searchTerm, setSearchTerm] = useState('');

  const byId = useMemo(
    () => new Map(discovery.workflows.map((workflow) => [workflow.id, workflow])),
    [discovery.workflows],
  );

  const entries: WorkflowEntry[] = useMemo<WorkflowEntry[]>(
    () => [
      ...selectedIds.filter((id) => !byId.has(id)).map((id) => ({ kind: 'missing' as const, id })),
      ...discovery.workflows.map((workflow) => ({ kind: 'discovered' as const, workflow })),
    ],
    [discovery.workflows, selectedIds, byId],
  );

  const visibleEntries = useMemo(() => {
    const term = searchTerm.trim().toLowerCase();
    if (!term) return entries;
    return entries.filter((entry) => {
      const name = entry.kind === 'discovered' ? entry.workflow.displayName : entry.id;
      const id = entry.kind === 'discovered' ? entry.workflow.id : entry.id;
      return name.toLowerCase().includes(term) || id.toLowerCase().includes(term);
    });
  }, [entries, searchTerm]);

  const toggle = (id: string) => {
    if (disabled) return;
    onChange(selectedIds.includes(id) ? selectedIds.filter((entry) => entry !== id) : [...selectedIds, id]);
  };

  const move = (index: number, delta: number) => {
    if (disabled) return;
    const target = index + delta;
    if (target < 0 || target >= selectedIds.length) return;
    const next = [...selectedIds];
    [next[index], next[target]] = [next[target], next[index]];
    onChange(next);
  };

  const remove = (id: string) => {
    if (disabled) return;
    onChange(selectedIds.filter((entry) => entry !== id));
  };

  const selectionLabel = (id: string) => byId.get(id)?.displayName ?? id;

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between gap-2">
        <Text variant={TextVariants.label} color={TextColors.primary}>
          {t('export.workflows.title')}
        </Text>
        <button
          type="button"
          aria-label={t('export.workflows.refresh')}
          className="p-1.5 bg-surface rounded-md text-text-secondary hover:bg-card-active transition-colors disabled:opacity-50"
          disabled={disabled || discovery.status === 'loading'}
          onClick={discovery.refresh}
        >
          <RefreshCw size={14} className={discovery.status === 'loading' ? 'animate-spin' : ''} />
        </button>
      </div>

      {discovery.status === 'loading' && (
        <Text variant={TextVariants.small} color={TextColors.secondary}>
          {t('export.workflows.loading')}
        </Text>
      )}
      {discovery.status === 'error' && (
        <div className="flex flex-col">
          <Text variant={TextVariants.small} color={TextColors.secondary}>
            {t('export.workflows.error')}
          </Text>
          {discovery.errorMessage && (
            <Text variant={TextVariants.small} color={TextColors.secondary}>
              {discovery.errorMessage}
            </Text>
          )}
        </div>
      )}

      {selectedIds.length > 0 && (
        <div className="flex flex-col gap-1">
          <Text variant={TextVariants.small} color={TextColors.secondary}>
            {t('export.workflows.selectedTitle')}
          </Text>
          <ul className="flex flex-col gap-1">
            {selectedIds.map((id, index) => (
              <li key={id} className="flex items-center justify-between gap-2 bg-surface rounded-md px-2 py-1">
                <Text variant={TextVariants.small} color={TextColors.primary}>
                  {`${index + 1}. ${selectionLabel(id)}`}
                </Text>
                <div className="flex items-center gap-1">
                  <button
                    type="button"
                    aria-label={`${t('export.workflows.moveUp')} ${selectionLabel(id)}`}
                    className="p-1 text-text-secondary rounded-md hover:bg-card-active transition-colors disabled:opacity-50"
                    disabled={disabled || index === 0}
                    onClick={() => move(index, -1)}
                  >
                    <ChevronUp size={14} />
                  </button>
                  <button
                    type="button"
                    aria-label={`${t('export.workflows.moveDown')} ${selectionLabel(id)}`}
                    className="p-1 text-text-secondary rounded-md hover:bg-card-active transition-colors disabled:opacity-50"
                    disabled={disabled || index === selectedIds.length - 1}
                    onClick={() => move(index, 1)}
                  >
                    <ChevronDown size={14} />
                  </button>
                  <button
                    type="button"
                    aria-label={`${t('export.workflows.remove')} ${selectionLabel(id)}`}
                    className="p-1 text-text-secondary rounded-md hover:bg-card-active transition-colors disabled:opacity-50"
                    disabled={disabled}
                    onClick={() => remove(id)}
                  >
                    <X size={14} />
                  </button>
                </div>
              </li>
            ))}
          </ul>
        </div>
      )}

      <input
        type="text"
        value={searchTerm}
        onChange={(event) => setSearchTerm(event.target.value)}
        placeholder={t('export.workflows.searchPlaceholder')}
        disabled={disabled}
        className="w-full bg-surface border border-surface rounded-md p-2 text-sm text-text-primary focus:ring-accent focus:border-accent"
      />

      {discovery.status === 'ready' && visibleEntries.length === 0 && (
        <Text variant={TextVariants.small} color={TextColors.secondary}>
          {t('export.workflows.noWorkflows')}
        </Text>
      )}

      <ul className="flex flex-col gap-1">
        {visibleEntries.map((entry) => {
          if (entry.kind === 'missing') {
            return (
              <li key={`missing-${entry.id}`} className="flex flex-col gap-0.5 px-2 py-1">
                <label className="flex items-center gap-2 opacity-50">
                  <input type="checkbox" checked disabled onChange={() => toggle(entry.id)} className="accent-accent" />
                  <Text variant={TextVariants.small} color={TextColors.primary}>
                    {entry.id}
                  </Text>
                  <Text variant={TextVariants.small} color={TextColors.secondary}>
                    {t('export.workflows.missingBadge')}
                  </Text>
                </label>
                <Text variant={TextVariants.small} color={TextColors.secondary}>
                  {t('export.workflows.reasonMissing')}
                </Text>
              </li>
            );
          }
          const workflow = entry.workflow;
          const invalidReason = workflow.selectable
            ? null
            : (workflow.diagnostics[0]?.message ?? t('export.workflows.reasonInvalid'));
          const runtimeReason =
            invalidReason === null && !workflow.runtime.available ? (workflow.runtime.unavailableReason ?? '') : null;
          const entryDisabled = disabled || !workflow.selectable || !workflow.runtime.available;
          return (
            <li key={workflow.id} className="flex flex-col gap-0.5 px-2 py-1">
              <label className="flex items-center gap-2">
                <input
                  type="checkbox"
                  checked={selectedIds.includes(workflow.id)}
                  disabled={entryDisabled}
                  onChange={() => toggle(workflow.id)}
                  className="accent-accent"
                />
                <Text variant={TextVariants.small} color={TextColors.primary}>
                  {workflow.displayName}
                </Text>
              </label>
              <div className="flex flex-wrap gap-x-2 pl-6">
                <Text variant={TextVariants.small} color={TextColors.secondary}>
                  {`${t(languageLabelKey(workflow.language))}${
                    workflow.runtime.version ? ` ${workflow.runtime.version}` : ''
                  }`}
                </Text>
                <Text variant={TextVariants.small} color={TextColors.secondary}>
                  {t(phaseLabelKey(workflow.phase))}
                </Text>
                <Text variant={TextVariants.small} color={TextColors.secondary}>
                  {t(sourceLabelKey(workflow.source))}
                </Text>
              </div>
              {invalidReason !== null && (
                <div className="flex flex-wrap gap-x-1 pl-6">
                  <Text variant={TextVariants.small} color={TextColors.secondary}>
                    {`${t('export.workflows.reasonInvalid')}:`}
                  </Text>
                  <Text variant={TextVariants.small} color={TextColors.secondary}>
                    {invalidReason}
                  </Text>
                </div>
              )}
              {runtimeReason !== null && (
                <div className="flex flex-wrap gap-x-1 pl-6">
                  <Text variant={TextVariants.small} color={TextColors.secondary}>
                    {`${t('export.workflows.reasonUnavailable')}:`}
                  </Text>
                  <Text variant={TextVariants.small} color={TextColors.secondary}>
                    {runtimeReason}
                  </Text>
                </div>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
