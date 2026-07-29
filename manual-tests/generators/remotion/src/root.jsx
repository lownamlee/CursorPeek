import React from 'react';
import {Composition} from 'remotion';
import {FixtureVideo} from './video.jsx';

export const FixtureRoot = () => {
  return (
    <Composition
      id="CursorPeekManualFixture"
      component={FixtureVideo}
      durationInFrames={72}
      fps={24}
      width={640}
      height={360}
    />
  );
};
