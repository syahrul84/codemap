<?php

namespace App\Filament\Resources\Malls;

use App\Filament\Resources\Malls\Pages\CreateMall;
use App\Filament\Resources\Malls\Pages\EditMall;
use App\Filament\Resources\Malls\Pages\ListMalls;
use App\Filament\Resources\Malls\Schemas\MallForm;
use App\Models\Mall;
use Filament\Resources\Resource;
use Filament\Schemas\Schema;
use Filament\Tables\Columns\TextColumn;
use Filament\Tables\Table;

class MallResource extends Resource
{
    protected static ?string $model = Mall::class;

    public static function form(Schema $schema): Schema
    {
        return MallForm::configure($schema);
    }

    public static function table(Table $table): Table
    {
        return $table->columns([TextColumn::make('name'), TextColumn::make('city')]);
    }

    public static function getPages(): array
    {
        return [
            'index' => ListMalls::route('/'),
            'create' => CreateMall::route('/create'),
            'edit' => EditMall::route('/{record}/edit'),
        ];
    }
}
