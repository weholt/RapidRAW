import { useEffect, useMemo, useRef, useState } from 'react';
import type { KeyboardEvent, PointerEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Adjustments, INITIAL_ADJUSTMENTS, Recipe } from '../../utils/adjustments';

type Channel = 'rgb' | 'red' | 'green' | 'blue';
type Level = Recipe['levels']['rgb'];
type LevelField = keyof Level;
type HistogramChannel = 'red' | 'green' | 'blue';

interface Props {
  adjustments: Adjustments;
  setAdjustments(value: Partial<Adjustments> | ((previous: Adjustments) => Adjustments)): void;
  histogram?: Partial<Record<HistogramChannel, number[]>> | null;
  onDragStateChange?: (dragging: boolean) => void;
}

interface NumberFieldProps {
  label: string;
  value: number;
  min: number;
  max: number;
  step?: number;
  disabled: boolean;
  onCommit(value: number): void;
}

const channels: Channel[] = ['rgb', 'red', 'green', 'blue'];
const histogramColors: Record<HistogramChannel, string> = {
  red: '#ff6666',
  green: '#65c879',
  blue: '#689cff',
};
const clamp = (value: number, min: number, max: number) => Math.max(min, Math.min(max, value));
const percent = (value: number) => `${(value / 255) * 100}%`;

function NumberField({ label, value, min, max, step = 1, disabled, onCommit }: NumberFieldProps) {
  const [draft, setDraft] = useState(String(value));
  const inputRef = useRef<HTMLInputElement>(null);
  const cancelled = useRef(false);

  useEffect(() => {
    if (document.activeElement !== inputRef.current) setDraft(String(value));
  }, [value]);

  const commit = () => {
    if (cancelled.current) {
      cancelled.current = false;
      setDraft(String(value));
      return;
    }
    const parsed = Number(draft);
    if (draft.trim() !== '' && Number.isFinite(parsed)) {
      const bounded = clamp(parsed, min, max);
      setDraft(String(bounded));
      onCommit(bounded);
    } else setDraft(String(value));
  };

  return (
    <input
      ref={inputRef}
      type="number"
      aria-label={label}
      title={label}
      min={min}
      max={max}
      step={step}
      value={draft}
      disabled={disabled}
      onChange={(event) => setDraft(event.target.value)}
      onBlur={commit}
      onKeyDown={(event) => {
        if (event.key === 'Enter') event.currentTarget.blur();
        if (event.key === 'Escape') {
          cancelled.current = true;
          setDraft(String(value));
          event.currentTarget.blur();
        }
      }}
      className="w-10 shrink-0 rounded-sm bg-bg-tertiary px-1 py-0.5 text-center text-xs tabular-nums text-text-primary [appearance:textfield] [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none disabled:opacity-50"
    />
  );
}

function histogramPath(data: number[], peak: number) {
  return data
    .map((count, index) => {
      const x = (index / Math.max(1, data.length - 1)) * 255;
      const y = 100 - Math.sqrt(Math.max(0, count) / peak) * 94;
      return `${index === 0 ? 'M' : 'L'}${x.toFixed(2)} ${y.toFixed(2)}`;
    })
    .join(' ');
}

export default function LevelsPanel({ adjustments, setAdjustments, histogram, onDragStateChange }: Props) {
  const { t } = useTranslation();
  const [channel, setChannel] = useState<Channel>('rgb');
  const trackRef = useRef<HTMLDivElement>(null);
  const activeDrag = useRef<LevelField | null>(null);
  const dragOffset = useRef(0);
  const onDragStateChangeRef = useRef(onDragStateChange);
  onDragStateChangeRef.current = onDragStateChange;

  useEffect(() => {
    return () => {
      if (activeDrag.current) onDragStateChangeRef.current?.(false);
    };
  }, []);

  const levels = adjustments.levels || INITIAL_ADJUSTMENTS.levels;
  const current = levels[channel];

  const labels: Record<LevelField, string> = {
    inputBlack: t('adjustments.levels.inputBlack'),
    inputWhite: t('adjustments.levels.inputWhite'),
    midtone: t('adjustments.levels.midtone'),
    outputBlack: t('adjustments.levels.outputBlack'),
    outputWhite: t('adjustments.levels.outputWhite'),
  };
  const channelLabels: Record<Channel, string> = {
    rgb: t('adjustments.levels.channels.rgb'),
    red: t('adjustments.levels.channels.red'),
    green: t('adjustments.levels.channels.green'),
    blue: t('adjustments.levels.channels.blue'),
  };

  const bounds = (field: LevelField, value: Level): [number, number] => {
    switch (field) {
      case 'inputBlack':
        return [0, value.inputWhite - 1];
      case 'inputWhite':
        return [value.inputBlack + 1, 255];
      case 'outputBlack':
        return [0, value.outputWhite - 1];
      case 'outputWhite':
        return [value.outputBlack + 1, 255];
      case 'midtone':
        return [-1, 1];
    }
  };

  const changeLevel = (field: LevelField, value: number) => {
    if (!Number.isFinite(value)) return;
    setAdjustments((previous) => {
      const previousLevels = previous.levels || INITIAL_ADJUSTMENTS.levels;
      const previousChannel = previousLevels[channel];
      const [min, max] = bounds(field, previousChannel);
      const bounded = clamp(field === 'midtone' ? Math.round(value * 100) / 100 : Math.round(value), min, max);
      return {
        ...previous,
        levels: {
          ...previousLevels,
          [channel]: { ...previousChannel, [field]: bounded },
        },
      };
    });
  };

  const updateFromPointer = (field: LevelField, clientX: number) => {
    const rect = trackRef.current?.getBoundingClientRect();
    if (!rect || rect.width === 0) return;
    const position = clamp(((clientX - rect.left) / rect.width) * 255, 0, 255);
    if (field === 'midtone') {
      const span = current.inputWhite - current.inputBlack;
      const fraction = (position - current.inputBlack) / span;
      changeLevel('midtone', (0.5 - fraction) / 0.45);
    } else {
      changeLevel(field, position);
    }
  };

  const endDrag = (event: PointerEvent<HTMLButtonElement>, field: LevelField) => {
    if (activeDrag.current !== field) return;
    activeDrag.current = null;
    if (event.currentTarget.hasPointerCapture?.(event.pointerId))
      event.currentTarget.releasePointerCapture(event.pointerId);
    onDragStateChange?.(false);
  };

  const handleKey = (event: KeyboardEvent<HTMLButtonElement>, field: LevelField) => {
    const [min, max] = bounds(field, current);
    const step = field === 'midtone' ? 0.01 : event.shiftKey ? 10 : 1;
    const direction = field === 'midtone' ? -1 : 1;
    let next: number;
    switch (event.key) {
      case 'ArrowRight':
      case 'ArrowUp':
        next = current[field] + step * direction;
        break;
      case 'ArrowLeft':
      case 'ArrowDown':
        next = current[field] - step * direction;
        break;
      case 'Home':
        next = field === 'midtone' ? max : min;
        break;
      case 'End':
        next = field === 'midtone' ? min : max;
        break;
      default:
        return;
    }
    event.preventDefault();
    changeLevel(field, next);
  };

  const histogramPaths = useMemo(() => {
    const data = Object.fromEntries(
      (['red', 'green', 'blue'] as const).map((color) => [
        color,
        Array.isArray(histogram?.[color]) ? histogram[color] : [],
      ]),
    ) as Record<HistogramChannel, number[]>;
    const peak = Math.max(1, ...Object.values(data).flat());
    const paths = Object.fromEntries(
      (['red', 'green', 'blue'] as const).map((color) => [color, histogramPath(data[color], peak)]),
    ) as Record<HistogramChannel, string>;
    const luma = data.red.map((count, index) => (count + (data.green[index] || 0) + (data.blue[index] || 0)) / 3);
    return { paths, luma: histogramPath(luma, peak) };
  }, [histogram]);

  const inputMid = current.inputBlack + (current.inputWhite - current.inputBlack) * (0.5 - 0.45 * current.midtone);
  const outputMid = (current.outputBlack + current.outputWhite) / 2;

  const handle = (field: LevelField, value: number, top: boolean, tone: 'black' | 'white' | 'midtone') => {
    const [min, max] = bounds(field, current);
    return (
      <button
        key={field}
        type="button"
        role="slider"
        aria-label={labels[field]}
        aria-orientation="horizontal"
        aria-valuemin={min}
        aria-valuemax={max}
        aria-valuenow={current[field]}
        disabled={!levels.enabled}
        data-testid={`levels-handle-${field}`}
        className="absolute z-20 flex h-5 w-5 -translate-x-1/2 items-center justify-center cursor-ew-resize touch-none disabled:cursor-not-allowed"
        style={{ left: percent(value), top: top ? 0 : 110 }}
        onPointerDown={(event) => {
          if (!levels.enabled) return;
          event.preventDefault();
          activeDrag.current = field;
          event.currentTarget.setPointerCapture?.(event.pointerId);
          onDragStateChange?.(true);
          const rect = trackRef.current?.getBoundingClientRect();
          dragOffset.current = rect ? rect.left + (value / 255) * rect.width - event.clientX : 0;
        }}
        onPointerMove={(event) => {
          if (activeDrag.current === field) updateFromPointer(field, event.clientX + dragOffset.current);
        }}
        onPointerUp={(event) => endDrag(event, field)}
        onPointerCancel={(event) => endDrag(event, field)}
        onLostPointerCapture={(event) => endDrag(event, field)}
        onKeyDown={(event) => handleKey(event, field)}
      >
        {tone === 'midtone' ? (
          <span className="h-2.5 w-2.5 rounded-full border border-white/70 bg-neutral-400" />
        ) : (
          <svg width="14" height="16" viewBox="0 0 14 16" aria-hidden="true">
            <path
              d={top ? 'M1 2 H13 L7 14 Z' : 'M1 14 H13 L7 2 Z'}
              fill={tone === 'white' ? '#f9fafb' : '#171717'}
              stroke="#d1d5db"
              strokeWidth="1.4"
              strokeLinejoin="round"
            />
          </svg>
        )}
      </button>
    );
  };

  return (
    <div className="space-y-2">
      <label className="flex items-center gap-2 text-xs text-text-secondary">
        <input
          type="checkbox"
          checked={levels.enabled}
          onChange={(event) =>
            setAdjustments((previous) => ({
              ...previous,
              levels: { ...(previous.levels || INITIAL_ADJUSTMENTS.levels), enabled: event.target.checked },
            }))
          }
        />
        {t('adjustments.levels.enabled')}
      </label>

      <div className="flex items-center justify-between">
        <NumberField
          key={`${channel}-inputBlack`}
          label={labels.inputBlack}
          value={current.inputBlack}
          min={0}
          max={current.inputWhite - 1}
          disabled={!levels.enabled}
          onCommit={(value) => changeLevel('inputBlack', value)}
        />
        <NumberField
          key={`${channel}-inputWhite`}
          label={labels.inputWhite}
          value={current.inputWhite}
          min={current.inputBlack + 1}
          max={255}
          disabled={!levels.enabled}
          onCommit={(value) => changeLevel('inputWhite', value)}
        />
      </div>
      <div className="flex gap-1" role="tablist" aria-label={t('editor.adjustments.sections.levels')}>
        {channels.map((name) => (
          <button
            key={name}
            type="button"
            role="tab"
            aria-selected={channel === name}
            onClick={() => setChannel(name)}
            className={`min-w-0 flex-1 border-b px-1 py-1 text-xs ${channel === name ? 'border-accent text-accent' : 'border-text-secondary/30 text-text-secondary hover:text-text-primary'}`}
          >
            {channelLabels[name]}
          </button>
        ))}
      </div>

      <div className="px-2">
        <div
          ref={trackRef}
          data-testid="levels-track"
          className={`relative h-[130px] ${levels.enabled ? '' : 'opacity-50'}`}
        >
          <div className="absolute inset-x-0 top-3 h-[104px] overflow-hidden rounded-sm bg-bg-tertiary">
            <svg
              className="h-full w-full"
              viewBox="0 0 255 100"
              preserveAspectRatio="none"
              role="img"
              aria-label={t('ui.waveform.tooltips.histogram')}
            >
              {[25, 50, 75].map((position) => (
                <g key={position} stroke="#ffffff" strokeOpacity="0.08" strokeWidth="0.5">
                  <line x1={position * 2.55} y1="0" x2={position * 2.55} y2="100" />
                  <line x1="0" y1={position} x2="255" y2={position} />
                </g>
              ))}
              {channel === 'rgb' && histogramPaths.luma && (
                <path d={`${histogramPaths.luma} L255 100 L0 100 Z`} fill="#c4c4c4" fillOpacity="0.18" />
              )}
              {(['red', 'green', 'blue'] as const).map((color) =>
                (channel === 'rgb' || channel === color) && histogramPaths.paths[color] ? (
                  <path
                    key={color}
                    data-testid={`levels-histogram-${color}`}
                    d={histogramPaths.paths[color]}
                    fill="none"
                    stroke={histogramColors[color]}
                    strokeWidth="1.4"
                    vectorEffect="non-scaling-stroke"
                  />
                ) : null,
              )}
            </svg>
          </div>

          <svg
            className="pointer-events-none absolute inset-0 h-full w-full"
            viewBox="0 0 255 130"
            preserveAspectRatio="none"
          >
            <line
              data-testid="levels-connector-black"
              x1={current.inputBlack}
              y1="9"
              x2={current.outputBlack}
              y2="120"
              stroke="#d1d5db"
              strokeOpacity="0.8"
              strokeWidth="1.3"
              vectorEffect="non-scaling-stroke"
            />
            <line
              data-testid="levels-connector-midtone"
              x1={inputMid}
              y1="9"
              x2={outputMid}
              y2="120"
              stroke="#b6b6b6"
              strokeOpacity="0.9"
              strokeWidth="1.3"
              vectorEffect="non-scaling-stroke"
            />
            <line
              data-testid="levels-connector-white"
              x1={current.inputWhite}
              y1="9"
              x2={current.outputWhite}
              y2="120"
              stroke="#f9fafb"
              strokeOpacity="0.9"
              strokeWidth="1.3"
              vectorEffect="non-scaling-stroke"
            />
          </svg>

          {handle('inputBlack', current.inputBlack, true, 'black')}
          {handle('midtone', inputMid, true, 'midtone')}
          {handle('inputWhite', current.inputWhite, true, 'white')}
          {handle('outputBlack', current.outputBlack, false, 'black')}
          <span
            aria-hidden="true"
            className="pointer-events-none absolute z-20 flex h-5 w-5 -translate-x-1/2 items-center justify-center"
            style={{ left: percent(outputMid), top: 110 }}
          >
            <svg width="14" height="16" viewBox="0 0 14 16">
              <path d="M1 14 H13 L7 2 Z" fill="#999999" stroke="#d1d5db" strokeWidth="1.4" />
            </svg>
          </span>
          {handle('outputWhite', current.outputWhite, false, 'white')}
        </div>
      </div>

      <div className="flex items-center justify-between gap-1">
        <NumberField
          key={`${channel}-outputBlack`}
          label={labels.outputBlack}
          value={current.outputBlack}
          min={0}
          max={current.outputWhite - 1}
          disabled={!levels.enabled}
          onCommit={(value) => changeLevel('outputBlack', value)}
        />
        <NumberField
          key={`${channel}-midtone`}
          label={labels.midtone}
          value={current.midtone}
          min={-1}
          max={1}
          step={0.01}
          disabled={!levels.enabled}
          onCommit={(value) => changeLevel('midtone', value)}
        />
        <NumberField
          key={`${channel}-outputWhite`}
          label={labels.outputWhite}
          value={current.outputWhite}
          min={current.outputBlack + 1}
          max={255}
          disabled={!levels.enabled}
          onCommit={(value) => changeLevel('outputWhite', value)}
        />
      </div>
    </div>
  );
}
