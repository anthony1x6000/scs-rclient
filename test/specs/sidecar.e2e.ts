import { expect } from '@wdio/globals';

describe('Tauri Application Sidecar Detection', () => {
  it('should verify the packaged rclone sidecar is being used', async () => {
    // Wait for the React application to render and run the detection logic
    await browser.pause(2000);

    // Retrieve the logs from the Webview's console
    // WebdriverIO allows fetching browser logs through getLogs
    const logs = await browser.getLogs('browser');
    
    // Check if any log contains the expected success message
    const sidecarLog = logs.find((log) => 
      log.message.includes('Using packaged rclone sidecar.')
    );
    
    // Also check if there were any 'os error 193' errors
    const errorLog = logs.find((log) => 
      log.message.includes('os error 193')
    );

    expect(errorLog).toBeUndefined();
    expect(sidecarLog).toBeDefined();
  });
});
