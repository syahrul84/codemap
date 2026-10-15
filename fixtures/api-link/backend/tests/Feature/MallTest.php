<?php

namespace Tests\Feature;

use App\Models\Mall;
use Tests\TestCase;

class MallTest extends TestCase
{
    public function test_lists_malls(): void
    {
        $this->getJson('/api/v1/malls')->assertOk();
    }

    public function test_shows_a_mall(): void
    {
        $mall = Mall::factory()->create();
        $this->getJson("/api/v1/malls/{$mall->slug}")->assertOk();
        $this->getJson(route('malls.show', 'abc'))->assertOk();
    }
}
