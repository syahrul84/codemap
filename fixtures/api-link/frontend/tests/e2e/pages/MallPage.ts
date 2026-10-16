import type { Page } from '@playwright/test'

export class MallPage {
  constructor(private page: Page) {}

  async goto(slug: string) {
    await this.page.goto(`/malls/${slug}`)
  }
}
