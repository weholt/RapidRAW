import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { ComponentType } from 'react';
import '../../../i18n';
import { Row } from './LibraryItems';

describe('capture-session rows', () => {
  it('renders a fixed-height, non-selectable accessible session heading', () => {
    const SessionRow = Row as ComponentType<Record<string, unknown>>;
    const onImageClick = () => {
      throw new Error('session headers must not be selectable');
    };
    render(
      <SessionRow
        index={0}
        style={{ height: 44, transform: 'translateY(0px)' }}
        rows={[
          {
            type: 'session-header',
            id: 'capture-one',
            startMs: Date.UTC(2024, 0, 1, 10),
            endMs: Date.UTC(2024, 0, 1, 10, 5),
            count: 2,
            fallbackCount: 1,
          },
        ]}
        activePath={null}
        multiSelectedSet={new Set()}
        onContextMenu={() => {}}
        onImageClick={onImageClick}
        onImageDoubleClick={() => {}}
        thumbnailAspectRatio="cover"
        onImageLoad={() => {}}
        imageRatings={{}}
        baseFolderPath="/photos"
        itemWidth={200}
        itemHeight={200}
        outerPadding={12}
        gap={12}
        isListView={false}
        columnWidths={{}}
        queueThumbnailRequest={() => {}}
        onToggleRecursiveFolder={() => {}}
        groupBadgeInfo={null}
      />,
    );

    const heading = screen.getByRole('heading', { level: 3 });
    expect(heading).toHaveAttribute('data-capture-session-id', 'capture-one');
    expect(heading).toHaveStyle({ height: '44px' });
    expect(screen.getByText('2 images')).toBeInTheDocument();
    expect(screen.getByLabelText('1 image uses its file modified time')).toBeInTheDocument();
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
  });
});
