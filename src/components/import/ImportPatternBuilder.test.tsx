import { useState } from 'react';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import type { ImportPattern } from '../ui/ExportImportProperties';
import ImportPatternBuilder from './ImportPatternBuilder';

function Harness() {
  const [pattern, setPattern] = useState<ImportPattern>({
    version: 1,
    missingTokenPolicy: 'fallback',
    parts: [{ type: 'literal', value: 'photos-' }],
  });
  return (
    <>
      <ImportPatternBuilder id="test" label="Filename pattern" pattern={pattern} onChange={setPattern} />
      <output data-testid="serialized">{JSON.stringify(pattern)}</output>
    </>
  );
}

describe('ImportPatternBuilder', () => {
  it('provides click insertion, editing, removal, and keyboard-equivalent ordering controls', async () => {
    const user = userEvent.setup();
    render(<Harness />);
    const addButtons = screen.getAllByRole('button', { name: 'modals.importSettings.addToken' });
    await user.click(addButtons[7]);
    expect(screen.getByTestId('serialized')).toHaveTextContent('"token":"make"');
    await user.type(screen.getByRole('textbox', { name: 'modals.importSettings.fallbackValue' }), 'unknown');
    expect(screen.getByTestId('serialized')).toHaveTextContent('"fallback":"unknown"');
    await user.click(screen.getAllByRole('button', { name: 'modals.importSettings.moveLeft' })[1]);
    expect(screen.getByTestId('serialized').textContent).toContain(
      '"parts":[{"type":"token","token":"make","fallback":"unknown"},{"type":"literal","value":"photos-"}]',
    );
    await user.click(screen.getAllByRole('button', { name: 'modals.importSettings.removeBlock' })[1]);
    expect(screen.getByTestId('serialized')).not.toHaveTextContent('photos-');
  });
});
