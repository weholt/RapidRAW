import {
  DndContext,
  KeyboardSensor,
  PointerSensor,
  useDraggable,
  useDroppable,
  useSensor,
  useSensors,
} from '@dnd-kit/core';
import type { DragEndEvent } from '@dnd-kit/core';
import type { ReactNode } from 'react';
import { GripVertical } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import type { ImportPattern, MetadataToken, PatternPart } from '../ui/ExportImportProperties';

export const IMPORT_TOKENS: MetadataToken[] = [
  'originalStem',
  'sequence',
  'year',
  'month',
  'day',
  'hour',
  'minute',
  'make',
  'model',
  'lensModel',
  'iso',
  'artist',
  'copyright',
  'imageDescription',
  'keywords',
  'rating',
  'colorLabel',
  'headline',
  'location',
];

interface Props {
  id: string;
  label: string;
  pattern: ImportPattern;
  onChange(pattern: ImportPattern): void;
}

function PatternStrip({ id, label, children }: { id: string; label: string; children: ReactNode }) {
  const { setNodeRef } = useDroppable({ id: `${id}:strip` });
  return (
    <div
      ref={setNodeRef}
      className="flex min-h-12 flex-wrap gap-2 rounded border border-dashed border-surface p-2"
      aria-label={label}
    >
      {children}
    </div>
  );
}

function PaletteToken({ token }: { token: MetadataToken }) {
  const { attributes, listeners, setNodeRef, transform } = useDraggable({ id: `palette:${token}` });
  return (
    <button
      ref={setNodeRef}
      type="button"
      className="rounded bg-surface px-2 py-1 text-xs focus:ring-2 focus:ring-accent"
      style={transform ? { transform: `translate3d(${transform.x}px, ${transform.y}px, 0)` } : undefined}
      {...listeners}
      {...attributes}
    >
      {token}
    </button>
  );
}

function PatternBlock({
  id,
  part,
  index,
  total,
  onEdit,
  onMove,
  onRemove,
}: {
  id: string;
  part: PatternPart;
  index: number;
  total: number;
  onEdit(part: PatternPart): void;
  onMove(from: number, to: number): void;
  onRemove(): void;
}) {
  const { t } = useTranslation();
  const drag = useDraggable({ id: `${id}:part:${index}` });
  const drop = useDroppable({ id: `${id}:slot:${index}` });
  return (
    <div
      ref={(node) => {
        drag.setNodeRef(node);
        drop.setNodeRef(node);
      }}
      className="flex min-w-28 items-center gap-1 rounded border border-surface bg-bg-primary p-1 focus-within:ring-2 focus-within:ring-accent"
      style={drag.transform ? { transform: `translate3d(${drag.transform.x}px, ${drag.transform.y}px, 0)` } : undefined}
    >
      <button
        type="button"
        aria-label={part.type === 'token' ? part.token : part.value}
        {...drag.listeners}
        {...drag.attributes}
      >
        <GripVertical aria-hidden="true" size={14} />
      </button>
      {part.type === 'literal' ? (
        <input
          aria-label={t('modals.importSettings.literalValue')}
          className="min-w-16 flex-1 bg-transparent px-1"
          value={part.value}
          onChange={(event) => onEdit({ type: 'literal', value: event.target.value })}
        />
      ) : (
        <>
          <span className="px-1 text-xs">{part.token}</span>
          <input
            aria-label={t('modals.importSettings.fallbackValue')}
            className="min-w-12 flex-1 bg-transparent px-1 text-xs"
            placeholder={t('modals.importSettings.fallback')}
            value={part.fallback ?? ''}
            onChange={(event) => onEdit({ ...part, fallback: event.target.value || null })}
          />
        </>
      )}
      <button
        type="button"
        aria-label={t('modals.importSettings.moveLeft')}
        disabled={index === 0}
        onClick={() => onMove(index, index - 1)}
      >
        ←
      </button>
      <button
        type="button"
        aria-label={t('modals.importSettings.moveRight')}
        disabled={index === total - 1}
        onClick={() => onMove(index, index + 1)}
      >
        →
      </button>
      <button type="button" aria-label={t('modals.importSettings.removeBlock')} onClick={onRemove}>
        ×
      </button>
    </div>
  );
}

export default function ImportPatternBuilder({ id, label, pattern, onChange }: Props) {
  const { t } = useTranslation();
  const sensors = useSensors(useSensor(PointerSensor), useSensor(KeyboardSensor));
  const updateParts = (parts: PatternPart[]) => onChange({ ...pattern, parts });
  const insertToken = (token: MetadataToken, index = pattern.parts.length) => {
    const parts = [...pattern.parts];
    parts.splice(index, 0, { type: 'token', token, fallback: null });
    updateParts(parts);
  };
  const move = (from: number, to: number) => {
    if (to < 0 || to >= pattern.parts.length) return;
    const parts = [...pattern.parts];
    const [part] = parts.splice(from, 1);
    parts.splice(to, 0, part);
    updateParts(parts);
  };
  const onDragEnd = (event: DragEndEvent) => {
    const active = String(event.active.id);
    const over = event.over ? String(event.over.id) : '';
    if (active.startsWith('palette:')) {
      const token = active.slice('palette:'.length) as MetadataToken;
      const index = over.startsWith(`${id}:slot:`) ? Number(over.slice(`${id}:slot:`.length)) : pattern.parts.length;
      insertToken(token, index);
    } else if (active.startsWith(`${id}:part:`) && over.startsWith(`${id}:slot:`)) {
      move(Number(active.slice(`${id}:part:`.length)), Number(over.slice(`${id}:slot:`.length)));
    }
  };
  const text = pattern.parts
    .map((part) =>
      part.type === 'literal' ? part.value : `{${part.token}${part.fallback ? `|${part.fallback}` : ''}}`,
    )
    .join('');

  return (
    <fieldset className="space-y-2">
      <legend className="font-semibold">{label}</legend>
      <DndContext sensors={sensors} onDragEnd={onDragEnd}>
        <div className="flex flex-wrap gap-1" aria-label={t('modals.importSettings.metadataTokens')}>
          {IMPORT_TOKENS.map((token) => (
            <span key={token} onDoubleClick={() => insertToken(token)}>
              <PaletteToken token={token} />
              <button type="button" className="sr-only" onClick={() => insertToken(token)}>
                {t('modals.importSettings.addToken', { token })}
              </button>
            </span>
          ))}
        </div>
        <PatternStrip id={id} label={label}>
          {pattern.parts.map((part, index) => (
            <PatternBlock
              id={id}
              index={index}
              key={`${part.type}-${index}`}
              part={part}
              total={pattern.parts.length}
              onEdit={(next) => updateParts(pattern.parts.map((value, i) => (i === index ? next : value)))}
              onMove={move}
              onRemove={() => updateParts(pattern.parts.filter((_, i) => i !== index))}
            />
          ))}
        </PatternStrip>
      </DndContext>
      <div className="flex gap-2">
        <button
          type="button"
          className="rounded bg-surface px-2 py-1 text-xs"
          onClick={() => updateParts([...pattern.parts, { type: 'literal', value: '' }])}
        >
          {t('modals.importSettings.addLiteral')}
        </button>
        <output className="truncate text-xs text-text-secondary" aria-label={t('modals.importSettings.patternText')}>
          {text || t('modals.importSettings.emptyPattern')}
        </output>
      </div>
    </fieldset>
  );
}
