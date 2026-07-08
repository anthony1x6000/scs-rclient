import type { Options } from '@wdio/types';
import path from 'path';

export const config: Options.Testrunner = {
  runner: 'local',
  specs: [
    './test/specs/**/*.e2e.ts'
  ],
  maxInstances: 1,
  capabilities: [{
    maxInstances: 1,
    browserName: 'wry',
    'tauri:options': {
      application: process.platform === 'win32' 
        ? path.join(__dirname, 'src-tauri', 'target', 'release', 'scs-rclient.exe')
        : path.join(__dirname, 'src-tauri', 'target', 'release', 'scs-rclient'),
    }
  }],
  services: [
    [
      'tauri',
      {
        appBinaryPath: process.platform === 'win32' 
          ? path.join(__dirname, 'src-tauri', 'target', 'release', 'scs-rclient.exe')
          : path.join(__dirname, 'src-tauri', 'target', 'release', 'scs-rclient'),
        // Use tauri-driver so we don't have to embed Rust test plugins into the app
        driverProvider: 'tauri-driver',
      },
    ],
  ],
  logLevel: 'info',
  bail: 0,
  waitforTimeout: 10000,
  connectionRetryTimeout: 120000,
  connectionRetryCount: 3,
  framework: 'mocha',
  reporters: ['spec'],
  mochaOpts: {
    ui: 'bdd',
    timeout: 60000
  },
}
