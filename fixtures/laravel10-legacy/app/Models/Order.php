<?php

namespace App\Models;

use Illuminate\Database\Eloquent\Factories\HasFactory;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\SoftDeletes;

class Order extends Model
{
    use HasFactory, SoftDeletes;

    public const STATUS_PENDING = 'pending';
    public const STATUS_PAID = 'paid';

    protected $fillable = ['customer_id', 'status', 'total_cents', 'paid_at'];

    protected $hidden = ['deleted_at'];

    protected $casts = [
        'paid_at' => 'datetime',
        'total_cents' => 'integer',
    ];

    protected $appends = ['total_formatted'];

    public function customer()
    {
        return $this->belongsTo(Customer::class);
    }

    public function items()
    {
        return $this->hasMany(OrderItem::class);
    }

    /**
     * Total as a formatted money string.
     */
    public function getTotalFormattedAttribute(): string
    {
        return money($this->total_cents);
    }

    public function setStatusAttribute(string $value): void
    {
        $this->attributes['status'] = strtolower($value);
    }

    public function scopePaid($query)
    {
        return $query->where('status', self::STATUS_PAID);
    }
}
