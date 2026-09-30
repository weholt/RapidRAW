import { useTranslation } from 'react-i18next';
import Slider from '../ui/Slider';
import { Adjustments, INITIAL_ADJUSTMENTS, Recipe } from '../../utils/adjustments';

interface Props {
  adjustments: Adjustments;
  setAdjustments(value: Partial<Adjustments> | ((previous: Adjustments) => Adjustments)): void;
  onDragStateChange?: (dragging: boolean) => void;
}

const colors = ['reds', 'oranges', 'yellows', 'greens', 'aquas', 'blues', 'purples', 'magentas'] as const;

export default function BlackWhitePanel({ adjustments, setAdjustments, onDragStateChange }: Props) {
  const { t } = useTranslation();
  const enabled = adjustments.blackWhiteEnabled ?? false;
  const mix = adjustments.blackWhiteMix || INITIAL_ADJUSTMENTS.blackWhiteMix;

  const changeMix = (index: number, value: number) => {
    setAdjustments((previous) => {
      const nextMix = [...(previous.blackWhiteMix || INITIAL_ADJUSTMENTS.blackWhiteMix)] as Recipe['blackWhiteMix'];
      nextMix[index] = Math.max(-100, Math.min(100, value));
      return { ...previous, blackWhiteMix: nextMix };
    });
  };

  return (
    <div className="space-y-3">
      <label className="flex items-center gap-2 text-sm text-text-primary">
        <input
          type="checkbox"
          checked={enabled}
          onChange={(event) => setAdjustments({ blackWhiteEnabled: event.target.checked })}
        />
        {t('adjustments.blackWhite.convert')}
      </label>
      <div className="rounded-md bg-bg-tertiary p-2">
        {colors.map((color, index) => (
          <Slider
            key={color}
            label={t(`adjustments.color.mixerColors.${color}`)}
            min={-100}
            max={100}
            step={1}
            value={mix[index]}
            disabled={!enabled}
            onChange={(event) => changeMix(index, Number(event.target.value))}
            onDragStateChange={onDragStateChange}
          />
        ))}
      </div>
    </div>
  );
}
