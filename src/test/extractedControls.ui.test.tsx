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
      <Panel
        adjustments={adjustments}
        setAdjustments={update}
        histogram={{
          red: Array.from({ length: 256 }, (_, index) => (index === 80 ? 100 : 0)),
          green: Array.from({ length: 256 }, (_, index) => (index === 120 ? 100 : 0)),
          blue: Array.from({ length: 256 }, (_, index) => (index === 160 ? 100 : 0)),
        }}
      />
      <output data-testid="recipe">{JSON.stringify(adjustments)}</output>
    </>
  );
}

const recipe = () => JSON.parse(screen.getByTestId('recipe').textContent || '{}') as Adjustments;

describe('RapidRAW extracted adjustment panels', () => {
  it('shows RGB histogram channels and connects corresponding input and output points', () => {
    render(<Fixture Panel={LevelsPanel} />);
    expect(screen.getByTestId('levels-histogram-red')).toHaveAttribute('d');
    expect(screen.getByTestId('levels-histogram-green')).toHaveAttribute('d');
    expect(screen.getByTestId('levels-histogram-blue')).toHaveAttribute('d');

    fireEvent.change(screen.getByRole('spinbutton', { name: 'Black point' }), { target: { value: '8' } });
    fireEvent.blur(screen.getByRole('spinbutton', { name: 'Black point' }));
    fireEvent.change(screen.getByRole('spinbutton', { name: 'White point' }), { target: { value: '208' } });
    fireEvent.blur(screen.getByRole('spinbutton', { name: 'White point' }));
    fireEvent.change(screen.getByRole('spinbutton', { name: 'Output black' }), { target: { value: '13' } });
    fireEvent.blur(screen.getByRole('spinbutton', { name: 'Output black' }));
    fireEvent.change(screen.getByRole('spinbutton', { name: 'Output white' }), { target: { value: '228' } });
    fireEvent.blur(screen.getByRole('spinbutton', { name: 'Output white' }));

    expect(screen.getByTestId('levels-connector-black')).toHaveAttribute('x1', '8');
    expect(screen.getByTestId('levels-connector-black')).toHaveAttribute('x2', '13');
    expect(screen.getByTestId('levels-connector-midtone')).toHaveAttribute('x1', '108');
    expect(screen.getByTestId('levels-connector-midtone')).toHaveAttribute('x2', '120.5');
    expect(screen.getByTestId('levels-connector-white')).toHaveAttribute('x1', '208');
    expect(screen.getByTestId('levels-connector-white')).toHaveAttribute('x2', '228');
  });

  it('keeps channels independent and limits the input black handle below input white', () => {
    render(<Fixture Panel={LevelsPanel} />);
    fireEvent.click(screen.getByRole('tab', { name: 'Red' }));
    fireEvent.change(screen.getByRole('spinbutton', { name: 'Black point' }), { target: { value: '240' } });
    fireEvent.blur(screen.getByRole('spinbutton', { name: 'Black point' }));
    expect(recipe().levels.red.inputBlack).toBe(240);
    expect(screen.getByRole('slider', { name: 'White point' })).toHaveAttribute('aria-valuemin', '241');
    fireEvent.keyDown(screen.getByRole('slider', { name: 'Black point' }), { key: 'End' });
    expect(recipe().levels.red.inputBlack).toBe(254);
    expect(recipe().levels.rgb.inputBlack).toBe(0);
  });

  it('drags an input handle without changing the output point and moves its connector', () => {
    render(<Fixture Panel={LevelsPanel} />);
    const track = screen.getByTestId('levels-track');
    track.getBoundingClientRect = () => ({ left: 0, width: 255 }) as DOMRect;
    const black = screen.getByRole('slider', { name: 'Black point' });

    fireEvent.pointerDown(black, { pointerId: 1, clientX: 0 });
    fireEvent.pointerMove(black, { pointerId: 1, clientX: 24 });
    fireEvent.pointerUp(black, { pointerId: 1, clientX: 24 });

    expect(recipe().levels.rgb.inputBlack).toBe(24);
    expect(recipe().levels.rgb.outputBlack).toBe(0);
    expect(screen.getByTestId('levels-connector-black')).toHaveAttribute('x1', '24');
    expect(screen.getByTestId('levels-connector-black')).toHaveAttribute('x2', '0');
  });

  it('uses the middle handle for the recipe midtone while the lower midpoint follows output endpoints', () => {
    render(<Fixture Panel={LevelsPanel} />);
    screen.getByTestId('levels-track').getBoundingClientRect = () => ({ left: 0, width: 255 }) as DOMRect;
    const middle = screen.getByRole('slider', { name: 'Midtone' });

    fireEvent.pointerDown(middle, { pointerId: 2, clientX: 127.5 });
    fireEvent.pointerMove(middle, { pointerId: 2, clientX: 100 });
    fireEvent.pointerUp(middle, { pointerId: 2, clientX: 100 });

    expect(recipe().levels.rgb.midtone).toBeCloseTo(0.24, 2);
    expect(Number(screen.getByTestId('levels-connector-midtone').getAttribute('x1'))).toBeCloseTo(100, 0);
    expect(screen.getByTestId('levels-connector-midtone')).toHaveAttribute('x2', '127.5');
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
