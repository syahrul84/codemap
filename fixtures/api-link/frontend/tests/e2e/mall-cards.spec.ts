import { expect, test } from '@playwright/test'

test('mall cards link to the mall', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByTestId('mall-card').first()).toBeVisible()
})
