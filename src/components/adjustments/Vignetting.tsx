import { useTranslation } from 'react-i18next';
import Slider from '../ui/Slider';
import { Adjustments, INITIAL_ADJUSTMENTS, Recipe } from '../../utils/adjustments';

interface Props {
  adjustments: Adjustments;
  setAdjustments(value: Partial<Adjustments> | ((previous: Adjustments) => Adjustments)): void;
  onDragStateChange?: (dragging: boolean) => void;
}

const methods: Recipe['vignetting']['method'][] = ['ellipticOnCrop', 'circularOnCrop', 'circular'];

export default function VignettingPanel({ adjustments, setAdjustments, onDragStateChange }: Props) {
  const { t } = useTranslation();
  const vignetting = adjustments.vignetting || INITIAL_ADJUSTMENTS.vignetting;
  const methodLabels: Record<Recipe['vignetting']['method'], string> = {
    ellipticOnCrop: t('adjustments.vignetting.methods.ellipticOnCrop'),
    circularOnCrop: t('adjustments.vignetting.methods.circularOnCrop'),
    circular: t('adjustments.vignetting.methods.circular'),
  };
  const change = (patch: Partial<Recipe['vignetting']>) =>
    setAdjustments((previous) => ({
      ...previous,
      vignetting: { ...(previous.vignetting || INITIAL_ADJUSTMENTS.vignetting), ...patch },
    }));

  return (
    <div className="space-y-3">
      <label className="flex items-center gap-2 text-sm text-text-primary">
        <input
          type="checkbox"
          checked={vignetting.enabled}
          onChange={(event) => change({ enabled: event.target.checked })}
        />
        {t('adjustments.vignetting.enabled')}
      </label>
      <div className="rounded-md bg-bg-tertiary p-2">
        <Slider
          label={t('adjustments.vignetting.amount')}
          min={-4}
          max={4}
          step={0.05}
          value={vignetting.amount}
          disabled={!vignetting.enabled}
          onChange={(event) => change({ amount: Number(event.target.value) })}
          onDragStateChange={onDragStateChange}
          suffix=" EV"
        />
        <label className="block text-sm text-text-secondary">
          {t('adjustments.vignetting.method')}
          <select
            className="mt-1 w-full rounded-md bg-surface p-2 text-text-primary"
            value={vignetting.method}
            disabled={!vignetting.enabled}
            onChange={(event) => change({ method: event.target.value as Recipe['vignetting']['method'] })}
          >
            {methods.map((method) => (
              <option key={method} value={method}>
                {methodLabels[method]}
              </option>
            ))}
          </select>
        </label>
      </div>
    </div>
  );
}
