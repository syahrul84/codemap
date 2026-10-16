import { expect, test } from '@playwright/test'

test('the cart opens', async ({ page }) => {
  await page.goto('/cart')
  await expect(page.getByTestId('cart-total')).toBeVisible()
})
