import { expect } from '@wdio/globals';

describe('Tauri Application Sidecar Detection', () => {
  it('should verify the packaged rclone sidecar is being used', async () => {
    // Wait for the React application to render and run the detection logic
    await browser.pause(2000);

    // Retrieve the detection status exposed to the window object
    // Since WebdriverIO v9 no longer supports getLogs('browser') in W3C mode,
    // we use a global variable set by the detection logic.
    const status = await browser.execute(() => (window as any).__TEST_SIDECAR_STATUS__);
    const errorMsg = await browser.execute(() => (window as any).__TEST_SIDECAR_ERROR__);
    
    // Check if there were any 'os error 193' errors during detection
    expect(errorMsg || '').not.toContain('os error 193');
    
    // Check if the packaged sidecar is being used
    expect(status).toBe('packaged');
  });
});
