<?php

namespace App\Http\Requests;

use Illuminate\Foundation\Http\FormRequest;
use Illuminate\Validation\Rule;

class StoreOrderRequest extends FormRequest
{
    public function authorize(): bool
    {
        return $this->user() !== null;
    }

    public function rules(): array
    {
        return [
            'customer_id' => 'required|exists:customers,id',
            'items' => ['required', 'array', 'min:1'],
            'items.*.sku' => 'required|string|max:64',
            'items.*.quantity' => ['required', 'integer', 'min:1'],
            'status' => ['sometimes', Rule::in(['pending', 'paid'])],
        ];
    }
}
