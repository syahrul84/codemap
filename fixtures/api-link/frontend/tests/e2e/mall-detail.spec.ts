import { expect, test } from '@playwright/test'
import { MallPage } from './pages/MallPage'

test('a mall shows its offers', async ({ page }) => {
  await new MallPage(page).goto('mid-valley')
  await expect(page.getByRole('heading')).toBeVisible()
})
