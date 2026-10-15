<?php

namespace App\Models;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Relations\BelongsToMany;

class Tag extends Model
{
    protected $fillable = ['name'];

    public function orders(): BelongsToMany
    {
        return $this->belongsToMany(Order::class, 'order_tag');
    }
}
