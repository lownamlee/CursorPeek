import {mkdir} from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {bundle} from '@remotion/bundler';
import {ensureBrowser, renderMedia, selectComposition} from '@remotion/renderer';

const directory = path.dirname(fileURLToPath(import.meta.url));
const outputDirectory = path.resolve(directory, '..', '..', 'videos');
const outputLocation = path.join(outputDirectory, 'sample.mp4');

await mkdir(outputDirectory, {recursive: true});
await ensureBrowser({logLevel: 'warn'});

const serveUrl = await bundle({
  entryPoint: path.join(directory, 'src', 'index.jsx'),
  webpackOverride: (configuration) => configuration,
});
const composition = await selectComposition({
  serveUrl,
  id: 'CursorPeekManualFixture',
  inputProps: {},
});

await renderMedia({
  codec: 'h264',
  composition,
  crf: 22,
  imageFormat: 'jpeg',
  inputProps: {},
  jpegQuality: 90,
  muted: true,
  outputLocation,
  overwrite: true,
  pixelFormat: 'yuv420p',
  serveUrl,
});

console.log(`Rendered ${outputLocation}`);
