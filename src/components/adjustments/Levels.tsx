import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import Slider from '../ui/Slider';
import { Adjustments, INITIAL_ADJUSTMENTS, Recipe } from '../../utils/adjustments';

type Channel = 'rgb' | 'red' | 'green' | 'blue';
type Level = Recipe['levels']['rgb'];
type LevelField = keyof Level;

interface Props {
  adjustments: Adjustments;
  setAdjustments(value: Partial<Adjustments> | ((previous: Adjustments) => Adjustments)): void;
  onDragStateChange?: (dragging: boolean) => void;
}

const channels: Channel[] = ['rgb', 'red', 'green', 'blue'];

export default function LevelsPanel({ adjustments, setAdjustments, onDragStateChange }: Props) {
  const { t } = useTranslation();
  const [channel, setChannel] = useState<Channel>('rgb');
  const levels = adjustments.levels || INITIAL_ADJUSTMENTS.levels;
  const current = levels[channel];

  const changeLevel = (field: LevelField, value: number) => {
    setAdjustments((previous) => {
      const previousLevels = previous.levels || INITIAL_ADJUSTMENTS.levels;
      const previousChannel = previousLevels[channel];
      const bounded =
        field === 'midtone'
          ? Math.max(-1, Math.min(1, value))
          : field === 'inputBlack'
            ? Math.max(0, Math.min(previousChannel.inputWhite - 1, value))
            : field === 'inputWhite'
              ? Math.max(previousChannel.inputBlack + 1, Math.min(255, value))
              : field === 'outputBlack'
                ? Math.max(0, Math.min(previousChannel.outputWhite - 1, value))
                : Math.max(previousChannel.outputBlack + 1, Math.min(255, value));
      return {
        ...previous,
        levels: {
          ...previousLevels,
          [channel]: { ...previousChannel, [field]: bounded },
        },
      };
    });
  };

  const controls: Array<{
    field: LevelField;
    label: string;
    min: number;
    max: number;
    step: number;
    defaultValue: number;
  }> = [
    {
      field: 'inputBlack',
      label: t('adjustments.levels.inputBlack'),
      min: 0,
      max: current.inputWhite - 1,
      step: 1,
      defaultValue: 0,
    },
    { field: 'midtone', label: t('adjustments.levels.midtone'), min: -1, max: 1, step: 0.01, defaultValue: 0 },
    {
      field: 'inputWhite',
      label: t('adjustments.levels.inputWhite'),
      min: current.inputBlack + 1,
      max: 255,
      step: 1,
      defaultValue: 255,
    },
    {
      field: 'outputBlack',
      label: t('adjustments.levels.outputBlack'),
      min: 0,
      max: current.outputWhite - 1,
      step: 1,
      defaultValue: 0,
    },
    {
      field: 'outputWhite',
      label: t('adjustments.levels.outputWhite'),
      min: current.outputBlack + 1,
      max: 255,
      step: 1,
      defaultValue: 255,
    },
  ];
  const channelLabels: Record<Channel, string> = {
    rgb: t('adjustments.levels.channels.rgb'),
    red: t('adjustments.levels.channels.red'),
    green: t('adjustments.levels.channels.green'),
    blue: t('adjustments.levels.channels.blue'),
  };

  return (
    <div className="space-y-3">
      <label className="flex items-center gap-2 text-sm text-text-primary">
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
      <div className="grid grid-cols-4 gap-1" role="tablist" aria-label={t('editor.adjustments.sections.levels')}>
        {channels.map((name) => (
          <button
            key={name}
            type="button"
            role="tab"
            aria-selected={channel === name}
            onClick={() => setChannel(name)}
            className={`rounded-md px-1 py-1.5 text-xs ${channel === name ? 'bg-accent text-button-text' : 'bg-bg-tertiary text-text-secondary hover:text-text-primary'}`}
          >
            {channelLabels[name]}
          </button>
        ))}
      </div>
      <div className="space-y-2 rounded-md bg-bg-tertiary p-2">
        {controls.map(({ field, label, min, max, step, defaultValue }, index) => (
          <div key={field}>
            {index === 0 || index === 3 ? (
              <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-text-secondary">
                {t(index === 0 ? 'adjustments.levels.input' : 'adjustments.levels.output')}
              </div>
            ) : null}
            <Slider
              label={label}
              min={min}
              max={max}
              step={step}
              defaultValue={defaultValue}
              value={current[field]}
              disabled={!levels.enabled}
              onChange={(event) => changeLevel(field, Number(event.target.value))}
              onDragStateChange={onDragStateChange}
            />
          </div>
        ))}
      </div>
    </div>
  );
}
