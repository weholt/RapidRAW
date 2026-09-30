import { useState } from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import '../i18n';
import LevelsPanel from '../components/adjustments/Levels';
import BlackWhitePanel from '../components/adjustments/BlackWhite';
import VignettingPanel from '../components/adjustments/Vignetting';
import { Adjustments, INITIAL_ADJUSTMENTS } from '../utils/adjustments';

type Setter = (value: Partial<Adjustments> | ((previous: Adjustments) => Adjustments)) => void;

function Fixture({ Panel }: { Panel: typeof LevelsPanel }) {
  const [adjustments, setState] = useState<Adjustments>(INITIAL_ADJUSTMENTS);
  const update: Setter = (value) =>
    setState((previous) => (typeof value === 'function' ? value(previous) : { ...previous, ...value }));
  return (
    <>
      <Panel adjustments={adjustments} setAdjustments={update} />
      <output data-testid="recipe">{JSON.stringify(adjustments)}</output>
    </>
  );
}

const recipe = () => JSON.parse(screen.getByTestId('recipe').textContent || '{}') as Adjustments;

describe('RapidRAW extracted adjustment panels', () => {
  it('shows independent Levels channels and keeps input endpoints ordered', () => {
    render(<Fixture Panel={LevelsPanel} />);
    fireEvent.click(screen.getByRole('tab', { name: 'Red' }));
    fireEvent.change(screen.getByRole('slider', { name: 'Black point' }), { target: { value: '240' } });
    expect(recipe().levels.red.inputBlack).toBe(240);
    expect(screen.getByRole('slider', { name: 'White point' })).toHaveAttribute('min', '241');
    expect(recipe().levels.rgb.inputBlack).toBe(0);
  });

  it('enables Black and White and updates the correct color mixer channel', () => {
    render(<Fixture Panel={BlackWhitePanel} />);
    expect(screen.getByRole('slider', { name: 'Reds' })).toBeDisabled();
    fireEvent.click(screen.getByRole('checkbox', { name: 'Convert to black and white' }));
    fireEvent.change(screen.getByRole('slider', { name: 'Reds' }), { target: { value: '35' } });
    expect(recipe().blackWhiteEnabled).toBe(true);
    expect(recipe().blackWhiteMix).toEqual([35, 0, 0, 0, 0, 0, 0, 0]);
  });

  it('sets an independent crop-aware vignette', () => {
    render(<Fixture Panel={VignettingPanel} />);
    fireEvent.change(screen.getByRole('slider', { name: 'Amount' }), { target: { value: '-1.5' } });
    fireEvent.change(screen.getByLabelText('Method'), { target: { value: 'circularOnCrop' } });
    expect(recipe().vignetting).toEqual({ enabled: true, amount: -1.5, method: 'circularOnCrop' });
  });
});
