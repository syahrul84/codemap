<?php

namespace App\Http\Resources;

use Illuminate\Http\Request;
use Illuminate\Http\Resources\Json\JsonResource;

class CartItemResource extends JsonResource
{
    public function toArray(Request $request): array
    {
        return [
            'id' => $this->id,
            'quantity' => $this->quantity,
            'max_per_order' => $this->listing->max_per_order,
            $this->mergeWhen($this->note !== null, ['note' => $this->note]),
        ];
    }
}
