<?php

namespace App\Filament\Resources\Malls\Schemas;

use Filament\Forms\Components\Select;
use Filament\Forms\Components\TextInput;
use Filament\Schemas\Components\Section;
use Filament\Schemas\Schema;

class MallForm
{
    public static function configure(Schema $schema): Schema
    {
        return $schema->components([
            Section::make('Mall')->schema([
                TextInput::make('name')->required(),
                Select::make('city'),
            ]),
        ]);
    }
}
