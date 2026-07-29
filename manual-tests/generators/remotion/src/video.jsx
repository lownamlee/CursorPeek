import React from 'react';
import {
  AbsoluteFill,
  interpolate,
  spring,
  useCurrentFrame,
  useVideoConfig,
} from 'remotion';

const palette = ['#60a5fa', '#34d399', '#f97316', '#a78bfa'];

export const FixtureVideo = () => {
  const frame = useCurrentFrame();
  const {durationInFrames, fps, width} = useVideoConfig();
  const progress = frame / (durationInFrames - 1);
  const entrance = spring({frame, fps, config: {damping: 16, stiffness: 110}});
  const orbitX = interpolate(progress, [0, 0.5, 1], [72, width - 72, 72]);
  const orbitY = 116 + Math.sin(progress * Math.PI * 4) * 34;
  const color = palette[Math.floor(frame / 18) % palette.length];

  return (
    <AbsoluteFill
      style={{
        background:
          'radial-gradient(circle at 50% 15%, #1d4ed8 0%, #172554 32%, #07111f 75%)',
        color: '#f8fafc',
        fontFamily: 'Segoe UI, Arial, sans-serif',
        overflow: 'hidden',
      }}
    >
      <div
        style={{
          position: 'absolute',
          left: orbitX - 34,
          top: orbitY - 34,
          width: 68,
          height: 68,
          borderRadius: 999,
          background: color,
          boxShadow: `0 0 38px ${color}`,
        }}
      />
      <div
        style={{
          position: 'absolute',
          inset: 28,
          border: '2px solid rgba(148, 163, 184, 0.45)',
          borderRadius: 28,
          background: 'rgba(8, 19, 32, 0.72)',
          transform: `scale(${0.88 + entrance * 0.12})`,
          opacity: entrance,
          padding: '42px 46px',
          display: 'flex',
          flexDirection: 'column',
          justifyContent: 'flex-end',
        }}
      >
        <div style={{fontSize: 19, color: '#93c5fd', letterSpacing: 4}}>CURSORPEEK</div>
        <div style={{fontSize: 44, fontWeight: 700, marginTop: 6}}>Video preview fixture</div>
        <div style={{fontSize: 18, color: '#cbd5e1', marginTop: 10}}>
          Remotion · 640 × 360 · 24 FPS · frame {String(frame + 1).padStart(2, '0')}
        </div>
        <div
          style={{
            height: 8,
            borderRadius: 999,
            background: '#1e293b',
            marginTop: 26,
            overflow: 'hidden',
          }}
        >
          <div
            style={{
              width: `${progress * 100}%`,
              height: '100%',
              background: color,
            }}
          />
        </div>
      </div>
    </AbsoluteFill>
  );
};
