<?php

namespace App\Filament\Pages;

use Filament\Forms\Components\TextInput;
use App\Support\ShopHours;
use Filament\Pages\Page;

class GeneralSettings extends Page
{
    protected static ?string $navigationLabel = 'General';

    protected static ?string $slug = 'settings/general';

    protected function getFormSchema(): array
    {
        return [TextInput::make('site_name'), TextInput::make('support_email')];
    }

    public function save(array $data): void
    {
        ShopHours::save((int) $data['open_hour']);
    }
}
