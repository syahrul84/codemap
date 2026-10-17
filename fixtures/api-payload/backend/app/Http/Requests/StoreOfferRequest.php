<?php

namespace App\Http\Requests;

use Illuminate\Foundation\Http\FormRequest;

class StoreOfferRequest extends FormRequest
{
    public function rules(): array
    {
        return [
            'price' => ['required', 'numeric'],
            'description' => 'required|string',
            'starts_at' => 'sometimes|required|date',
            'tags.*' => 'string',
        ];
    }
}
