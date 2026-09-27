import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it } from 'vitest';

const add = (left: number, right: number) => left + right;

function TestButton() {
  const [count, setCount] = useState(0);
  return (
    <button aria-label={`Count: ${count}`} onClick={() => setCount((value) => value + 1)}>
      {count}
    </button>
  );
}

describe('test foundation', () => {
  it('runs pure TypeScript tests', () => {
    expect(add(2, 3)).toBe(5);
  });

  it('runs React interaction tests in jsdom', async () => {
    const user = userEvent.setup();
    render(<TestButton />);

    await user.click(screen.getByRole('button', { name: 'Count: 0' }));

    expect(screen.getByRole('button', { name: 'Count: 1' }).textContent).toBe('1');
  });
});
