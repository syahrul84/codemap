<?php

namespace App\Models;

use Illuminate\Database\Eloquent\Model;

class Invoice extends Model
{
    protected $fillable = ['number', 'amount', 'due_on'];

    protected $casts = [
        'amount' => 'decimal:2',
        'due_on' => 'date',
    ];
}
